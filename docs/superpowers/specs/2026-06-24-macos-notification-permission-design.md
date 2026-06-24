# macOS 通知权限引导（第 4 个权限）设计

- 日期：2026-06-24
- 范围：前端（`web/`），后端零改动
- 状态：已通过用户评审，待实现

## 1. 背景与动机

cc-partner 的 macOS 欢迎页与设置页当前引导 3 个 **TCC 隐私权限**：

| 权限 | 消费者 | 检测方式 |
|------|--------|----------|
| screenCapture（屏幕录制） | 区域截图（xcap） | Rust FFI `CGPreflightScreenCaptureAccess` |
| accessibility（辅助功能） | 健康提醒读取前台窗口标题（active-win-pos-rs / AX API） | Rust FFI `AXIsProcessTrusted` |
| inputMonitoring（输入监控） | 健康提醒键鼠采样（device_query / IOHIDManager） | Rust FFI `CGEventTapCreate` 探测 |

应用需要发送**系统通知**（健康提醒的久坐/喝水提醒等），而 macOS 发送通知需用户授权「通知」权限。当前通知权限**未在欢迎页/设置页引导**。本设计把「通知」作为**第 4 个权限**纳入引导流程。

### 现状

- 通知权限的**依赖已就位**：`Cargo.toml`（`tauri-plugin-notification = "2"`）、`web/package.json`（`@tauri-apps/plugin-notification ^2.3.3`）、`capabilities/default.json`（`notification:default`）、`lib.rs` 插件注册均已配置。
- 通知的**消费侧尚未落地**：`web/CLAUDE.md` 描述的 `HealthReminderListener`（监听 `health:reminder` 后用 `sendNotification` 弹通知）在 `App.tsx` 中**不存在**；后端 `health:reminder` emit 与 `notifyEnabled` 配置已就位。本次只做权限引导，不实现消费侧。

## 2. 关键决策（已与用户确认）

1. **检测/请求机制：前端 JS API**（`isPermissionGranted` / `requestPermission`），**后端零改动**。
2. **不阻塞「继续使用」**：通知权限未授权时仍可继续，仅作为第 4 张卡片展示引导。
3. **平台范围：仅 macOS**，非 macOS 视为已授权（与现有 3 个 TCC 权限语义一致）。

### 为何选前端 JS API 而非后端统一

- 现有 3 个权限是 **TCC 隐私权限**：只能用 Rust FFI 检测，请求时打开「系统设置 → 隐私与安全」面板（accessibility/inputMonitoring）或弹系统框（screenCapture）。应用只能查询状态，不能直接改变。
- **通知权限不属于 TCC**：通过 `@tauri-apps/plugin-notification` 的 JS API 管理，`requestPermission()` 弹系统通知授权框。
- tauri-plugin-notification 的 `check_permissions`/`request_permissions` 在桌面端是 **mobile-only** API（`run_mobile_plugin`），桌面 macOS 通知权限检测/请求的可用 API 主要在 JS 侧。
- 因此通知权限走前端 JS API 最干净，保持后端 TCC 命令语义纯粹，风险最低。

## 3. 数据流与类型

### 后端（`src-tauri/`）：零改动

`PermissionsStatus`(Rust)、`check_permissions`、`request_permission` 命令保持不变，专注 3 个 TCC 权限。通知权限不混入 Rust 层。

### 前端类型（`web/src/lib/types.ts`）

```ts
export interface PermissionsStatus {
  screenCapture: { granted: boolean };
  inputMonitoring: { granted: boolean };
  accessibility: { granted: boolean };
  notification: { granted: boolean };   // 新增：前端 JS API 检测合并
}

export type PermissionType =
  | 'screenCapture'
  | 'inputMonitoring'
  | 'accessibility'
  | 'notification';   // 新增
```

> 注：后端 `check_permissions` 返回的对象不含 `notification` 字段，由 `usePermissions` hook 合并补齐。前端 `PermissionsStatus` 比后端 Rust `PermissionsStatus` 多一个 `notification` 字段，语义上「前端是用户需授权的全部权限视图，后端是 FFI 可检测的 TCC 权限」。

## 4. 平台检测 helper（新增 `web/src/lib/platform.ts`）

复用现有 `shortcutRecorder.ts::getDefaultShortcutValue` 已采用的 `globalThis.navigator?.platform` 判断模式（规则 9 复用，不重复造逻辑）：

```ts
/** 是否运行在 macOS（复用 navigator.platform 判断，与 shortcutRecorder 一致） */
export function isMacos(): boolean {
  return (globalThis.navigator?.platform ?? '').toLowerCase().includes('mac');
}
```

`shortcutRecorder.getDefaultShortcutValue` 可改为复用 `isMacos()`（可选优化，规则 9）。

## 5. `usePermissions` hook 改造（`web/src/hooks/usePermissions.ts`，核心）

### 轮询合并

```ts
// refresh：同时拉 TCC 权限（后端命令）+ 通知权限（JS API），合并
const refresh = useCallback(async () => {
  try {
    const [tcc, notifyGranted] = await Promise.all([
      configApi.permissions(),
      checkNotificationGranted(),   // macOS 调 isPermissionGranted()，非 macOS 返回 true
    ]);
    setStatus({ ...tcc, notification: { granted: notifyGranted } });
  } catch {
    // 接口失败保持当前状态，下轮重试
  }
}, []);
```

`checkNotificationGranted()` 与下文的 `requestNotificationPermission()` 均由新增模块 `web/src/lib/notification.ts` 导出（单一职责、hook 与 Settings 共用、可单测），消除「hook 内闭包 vs 共享模块」的归属歧义：

```ts
// web/src/lib/notification.ts
import { isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification';
import { isMacos } from './platform';

/** 查询通知授权状态：macOS 调 isPermissionGranted()，非 macOS 视为已授权 */
export async function checkNotificationGranted(): Promise<boolean> {
  if (!isMacos()) return true;                 // 非 macOS 视为已授权
  try {
    return await isPermissionGranted();        // @tauri-apps/plugin-notification
  } catch {
    return true;                               // 探测失败保守视为已授权，不阻断主流程
  }
}

/** 请求通知权限：macOS 调 requestPermission()，非 macOS no-op */
export async function requestNotificationPermission(): Promise<void> {
  if (!isMacos()) return;
  try {
    await requestPermission();                 // 返回 'granted'|'denied'|'default'，状态由轮询反映
  } catch {
    // 请求失败静默，轮询反映真实状态
  }
}
```

`usePermissions` 从 `lib/notification.ts` import 这两个函数。

### `allGranted`：保持只看 3 个 TCC 权限（不阻塞语义）

```ts
const allGranted =
  !!status &&
  status.screenCapture.granted &&
  status.accessibility.granted &&
  status.inputMonitoring.granted;
// 注意：不含 status.notification.granted —— 通知是可选功能，未授权不锁定「继续使用」
```

轮询停止判定（`stopWhenGranted`）也只看 3 个 TCC 权限（与 `allGranted` 一致）。

### `requestMissing`：区分权限类型

```ts
const requestMissing = useCallback(async () => {
  const current = statusRef.current;
  if (!current) return;
  const tasks: Promise<unknown>[] = [];
  if (!current.screenCapture.granted) tasks.push(configApi.requestPermission('screenCapture'));
  if (!current.accessibility.granted) tasks.push(configApi.requestPermission('accessibility'));
  if (!current.inputMonitoring.granted) tasks.push(configApi.requestPermission('inputMonitoring'));
  if (!current.notification.granted) tasks.push(requestNotificationPermission());
  if (tasks.length === 0) return;
  await Promise.all(tasks);
  await refresh();
}, [refresh]);
```

`requestNotificationPermission()` 由 `lib/notification.ts` 提供（见上）。

## 6. `mapPermissions`（`web/src/lib/permissionEntries.tsx`）

新增第 4 条 `notification`，置于末尾：

```tsx
{
  id: 'notification',
  icon: <BellIcon />,
  title: t('permission.notification.title'),
  description: t('permission.notification.description'),
  granted: status.notification.granted,
},
```

顺序：屏幕录制 → 辅助功能 → 输入监控 → **通知**（不破坏现有顺序）。

## 7. 图标：新增 `BellIcon`（`web/src/lib/icons`）

icons 库当前无铃铛图标，按现有 SVG 图标组件格式（与其他 `*Icon` 一致）新增一个。

## 8. 各页面集成

| 位置 | 改动 |
|------|------|
| **Welcome**（`pages/Welcome/Welcome.tsx`） | 零逻辑改动：通知卡片随 `mapPermissions` 自动出现；「继续使用」仍只看 `allGranted`（3 TCC）。 |
| **Settings 权限 Card**（`pages/Settings/Settings.tsx`） | JSX 零改动（自动多一条）；`handleRequestAccess(type)` 区分类型——`notification` 走 JS `requestPermission()`，其余走 `configApi.requestPermission`。 |
| **PermissionStatusBadge**（侧栏徽标） | 零改动：`allGranted` 不含通知，通知未授权时徽标不显示（符合「可选」语义）。 |

### Settings `handleRequestAccess` 改造

```ts
const handleRequestAccess = useCallback(
  async (type: PermissionType) => {
    try {
      if (type === 'notification') {
        await requestNotificationPermission();   // JS API（与 hook 内同源逻辑）
      } else {
        await configApi.requestPermission(type);
      }
      await refreshPermissions();
    } catch {
      // 请求失败静默，轮询会持续反映真实状态
    }
  },
  [refreshPermissions],
);
```

> `requestNotificationPermission` 直接从 `lib/notification.ts` import（与 hook 同源，规则 9 复用，避免 Settings 与 hook 各写一份）。

## 9. i18n（`web/src/i18n/locales/{en,zh}/welcome.json`）

新增 `permission.notification.{title,description}`：

| 语言 | title | description |
|------|-------|-------------|
| zh | 通知 | 接收健康提醒等系统通知 |
| en | Notifications | Receive system notifications like health reminders |

## 10. 边缘情况

- **macOS 通知被用户在系统设置手动关闭**：`isPermissionGranted` 返回 `false`，`requestPermission` 返回 `'denied'` 且不再弹框。卡片显示未授权，「去设置」点击为 no-op；描述文案提示用户可去「系统设置 → 通知」开启。MVP 不做 deep link（macOS 无稳定的「直达单 app 通知设置」URL scheme）。
- **`isPermissionGranted` / `requestPermission` 抛异常**：保守视为已授权 / 静默忽略，不阻断主流程（通知是可选功能）。
- **非 macOS**：`isMacos()` 为 false，`notification.granted` 恒 `true`，卡片显示「已授权」，不发起 JS API 调用。

## 11. 测试

- **单测**：
  - `isMacos()`：mock `navigator.platform`，覆盖 MacIntel/Win32/空串。
  - `usePermissions` 合并逻辑：mock `configApi.permissions` 与 `isPermissionGranted`，断言合并后 `status.notification.granted` 正确；`requestMissing` 对 `notification` 走 JS API、其余走 `configApi`。
- **现有测试**：`shortcutRecorder.test.ts`、`settingsState.test.ts` 不受影响。
- **手动验证（需用户介入，macOS）**：通知卡片显示；点「去设置」弹系统授权框；授权后 2s 内状态刷新为「已授权」；非 macOS 不展示通知卡片（或展示为已授权）。

## 12. CLAUDE.md 更新（规则 5）

更新 `web/CLAUDE.md`：

- 「macOS 权限流程」节：3 权限 → 4 权限，注明「通知」走前端 JS API（`isPermissionGranted`/`requestPermission`）、不阻塞「继续使用」、仅 macOS 引导。
- 「自定义 Hook」节 `usePermissions` 描述：补充通知权限合并逻辑。

`src-tauri/CLAUDE.md` 无需改动（后端零改动）。

## 13. 改动文件清单

**前端（`web/src/`）：**

1. `lib/types.ts` — `PermissionsStatus` 加 `notification`、`PermissionType` 加 `'notification'`
2. `lib/platform.ts` — **新增** `isMacos()` helper
3. `lib/notification.ts` — **新增** `checkNotificationGranted()` + `requestNotificationPermission()`（hook 与 Settings 共用）
4. `lib/icons` — 新增 `BellIcon`
5. `lib/permissionEntries.tsx` — `mapPermissions` 加第 4 条 notification
6. `hooks/usePermissions.ts` — 轮询合并、`requestMissing` 区分类型（import `lib/notification.ts`）
7. `pages/Settings/Settings.tsx` — `handleRequestAccess` 区分 notification（import `lib/notification.ts`）
8. `i18n/locales/zh/welcome.json` — `permission.notification.*`
9. `i18n/locales/en/welcome.json` — `permission.notification.*`

**文档：**

10. `web/CLAUDE.md` — 权限流程 + hook 描述更新

**后端：零改动。**

## 14. 规模与实现方式

约 100–140 行，分布在 10 个文件。强关联单一功能、文件间耦合高，适合**单个 subagent 串行实现**（非多 subagent 并行）。改动 < 一定规模且聚焦，按规则 6/14 评估后可直接在当前分支实现（或视需要用 worktree，由实现阶段决定）。

## 15. 未纳入范围（相关功能，规则 19）

通知的**消费侧**（`HealthReminderListener` 监听 `health:reminder` → `sendNotification` 弹系统通知）尚未实现。本次只做权限引导。是否补上消费侧让授权的通知真正派上用场，待用户确认后另行处理。
