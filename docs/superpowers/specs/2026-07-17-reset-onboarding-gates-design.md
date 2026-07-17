# 重置 Onboarding 门闩（LAN 披露 + 权限引导）

**日期**: 2026-07-17  
**状态**: 已批准（方案 2）  
**范围**: Settings → 常规新增「重置首次启动引导」；不清理业务内容数据

## 背景

macOS「全新安装」应用二进制后，用户数据仍会保留：

- `~/.cc-partner/gui-bootstrap.json`（LAN 风险披露已确认）
- WebView `localStorage['cp-permission-onboarded']`（权限 Welcome 已完成）

因此用户不会再次看到首次启动引导。需要在设置中提供显式重置入口，复现首次流程，且**不**删除 Prompt / 速记本 / 本地 Claude Code 相关内容。

## 目标

1. 在 **设置 → 常规** 提供危险操作按钮「重置首次启动引导」。
2. 用户确认后：
   - 清除 LAN 披露确认态；
   - 停止本机 backend/sidecar（若在运行）；
   - 清除权限 onboarding 标记；
   - **退出 GUI**，下次启动按首次流程：LAN 披露 →（三项 TCC 未齐则）`/welcome`。
3. 明确告知：应用**不能**撤销 macOS 系统 TCC（屏幕录制 / 输入监控 / 辅助功能 / 通知）；若需系统层拒绝权限，用户须在「系统设置 → 隐私与安全」手动关闭。

## 非目标

- 不清理 Prompt、速记本、Claude 历史、Claude Code 资产、CLAUDE.md 推送数据。
- 不清理 / 重置 `data.db` 其它业务表。
- 不重置 `config.json`（设备名、接收目录、快捷键等）。
- 不清理主题 / 语言 / 当前工作台项目等其它 localStorage。
- 不自动打开系统设置或批量 request 权限。
- 不实现「就地重进引导而不退出进程」。

## 行为契约

### 会清除

| 项 | 位置 | 方式 |
| --- | --- | --- |
| LAN 披露确认 | `gui-bootstrap.json` | 写回 `GuiBootstrapState::default()`（`lanDisclosureVersion=0`，`acknowledgedAt=None`），复用既有原子写 |
| 运行中 backend | sidecar / control API | 与「前后端都关闭」路径同语义的 stop，幂等 |
| 权限 onboarding 标记 | WebView localStorage | 删除 `cp-permission-onboarded`（`PERMISSION_ONBOARDED_KEY`） |

### 不会清除

- Prompt / 速记本 / Claude Code 相关内容与资产
- 业务数据库其它域
- 应用配置与 UI 偏好（除上述 onboarding 标记）
- 系统 TCC 授权状态

### 成功后生命周期

```
用户确认
  → invoke reset_onboarding_gates（写 bootstrap + stop backend）
  → localStorage.removeItem(PERMISSION_ONBOARDED_KEY)
  → best-effort flushPendingWrites（失败不阻断）
  → exit_gui
用户再次打开 app
  → LanDisclosureGate: required=true
  → 用户确认后 ensure/start sidecar
  → OnboardingGuard: 无标记且权限未齐 → /welcome
```

### 失败语义

- bootstrap 写入失败：**不** stop backend，返回错误，前端不退出、不删 localStorage。
- stop 失败：视为命令失败（bootstrap 已写回未确认时，下次仍会要求披露；文案展示错误，不 exit）。
- localStorage 清除失败：极少见；若 invoke 已成功，仍应尝试 exit（门闩后端侧已重置）。
- exit 失败：展示错误，用户可手动退出。

## 架构

### 后端

新增可测 helper（建议）与 Tauri 命令：

```text
gui_bootstrap::reset_lan_disclosure() -> Result<GuiBootstrapState>
  // save_gui_bootstrap(&GuiBootstrapState::default())

#[tauri::command]
reset_onboarding_gates(...) -> Result<ResetOnboardingGatesResult>
  // 1) reset_lan_disclosure()
  // 2) stop backend（复用现有 lifecycle / stop 路径，幂等）
  // 3) 返回 { ok, lanDisclosureReset, backendStopped }
```

**DTO（camelCase）**:

```json
{
  "ok": true,
  "lanDisclosureReset": true,
  "backendStopped": true
}
```

- 注册于 `lib.rs` invoke_handler（与 `get_lan_disclosure_status` / `acknowledge_lan_disclosure_and_start_backend` 同区）。
- 禁止扫描或删除 `data.db`、prompts 表、scratchpad、claude assets。
- 单测：临时目录写入已确认 bootstrap → reset → `is_acknowledged_for_version == false`；stop mock 幂等。

### 前端

| 层 | 职责 |
| --- | --- |
| `backendApi.resetOnboardingGates()` | invoke 封装 |
| `useSettingsController`（或现有 form/update 域 hook） | Dialog 状态、busy、错误、编排顺序 |
| `SettingsGeneralPanel` | pure UI：Card + Dialog + danger 按钮；**禁止** `@/api/*` |
| i18n `settings:onboardingReset.*` | zh/en 同步 |

编排顺序（严格）:

1. `resetOnboardingGates()`
2. 成功后 `localStorage.removeItem(PERMISSION_ONBOARDED_KEY)`
3. best-effort `pendingWrites.flushAll()`
4. `backendApi.exitGui()`

与现有「清除 Agent 历史」同一交互模式（Dialog + danger + busy 门闩；busy 时禁用 Escape/backdrop 关闭）。

### 文案要点（confirmBody）

- 将清除：LAN 风险披露确认、权限引导完成标记；并停止本机后台服务、退出应用。
- 不会清除：Prompt、速记本、Claude Code 相关内容、设备配置。
- 系统隐私权限不会自动撤销；如需系统层关闭，请到「系统设置 → 隐私与安全」操作。
- 下次启动将重新出现首次引导。

## 测试计划

1. **Rust unit**: bootstrap 重置语义；stop 幂等；不触碰业务路径（命令实现层面可测 helper）。
2. **前端 unit**:  
   - 确认路径调用顺序（reset → removeItem → exitGui）；  
   - reset 失败不 exit、不 removeItem；  
   - `settingsOwnership`：General panel 无 api import。
3. **手工**: 本机确认后重启，应先见 LAN gate，再视权限状态进入 Welcome。

## 文档同步（实现时）

- `web/CLAUDE.md`：Settings 常规 onboarding 重置 + 退出语义。
- `src-tauri/CLAUDE.md`：`reset_onboarding_gates` 与 bootstrap 重置边界（可选短节）。
- 不新增总结类 md；本文件为设计规格。

## 实现约束（项目规则）

- React hooks 全在 early return 之前。
- 非 DB schema 变更，无需向后兼容旧引导标记格式。
- 改动预计超过 100 行时，用 worktree + subagent 并行（Rust 命令 / 前端 Settings / 测试）。
- 函数中文 docstring（Business Logic / Code Logic）。
- 编码实现 subagent 使用 `model: sonnet`；规划已完成。

## 验收标准

- [ ] 设置 → 常规可见重置入口与确认 Dialog。
- [ ] 确认成功后应用退出；重启后 `get_lan_disclosure_status.required === true`。
- [ ] 重启后无 `cp-permission-onboarded` 时，若三项 TCC 未齐，进入 `/welcome`。
- [ ] Prompt / 速记本 / Claude Code 相关数据仍在。
- [ ] 单元测试与 ownership 合同通过。
