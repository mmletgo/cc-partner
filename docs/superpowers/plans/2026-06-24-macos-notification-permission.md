# macOS 通知权限引导（第 4 个权限）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 macOS 欢迎页与设置页把「通知」作为第 4 个权限纳入引导授权（前端 JS API 检测/请求，不阻塞「继续使用」，仅 macOS，后端零改动）。

**Architecture:** 通知权限不属于 TCC，用 `@tauri-apps/plugin-notification` 的 `isPermissionGranted`/`requestPermission`（JS API）检测/请求，封装在新增 `lib/notification.ts`；`usePermissions` hook 轮询时把通知状态合并进统一的 `PermissionsStatus`；`mapPermissions` 多渲染一条通知卡片。现有 3 个 TCC 权限的后端命令与 `allGranted`（继续使用判定）保持不变。

**Tech Stack:** React 19 + TypeScript + Vite、`@tauri-apps/plugin-notification ^2.3.3`（已装）、`@tauri-apps/api`、react-i18next、Tauri 2。

## Global Constraints

（摘自 spec，每个任务的需求隐含包含本节）

- **仅前端**：改动全部在 `web/`，**后端（`src-tauri/`）零改动**。
- **通知权限走前端 JS API**：`isPermissionGranted`/`requestPermission`，不混入 Rust `check_permissions`/`request_permission` 命令。
- **不阻塞「继续使用」**：`allGranted` 与 Welcome 轮询停止判定**只看 3 个 TCC 权限**（screenCapture/accessibility/inputMonitoring），不含 notification。
- **仅 macOS 引导**：非 macOS `isMacos()` 返回 false → 通知视为已授权、不发 JS API 调用。
- **代码 UTF-8 编码**；新增/修改函数加中文注释，格式为 `Business Logic（为什么）` + `Code Logic（做什么）` 双段（项目规则）。
- **i18n**：用户可见文案走 `welcome` ns 的 `permission.notification.*`，禁止组件内硬编码中英文字面量（en + zh 两份都要加）。
- **hooks 顺序**：`usePermissions` 改动后所有 hooks 仍在 early return 之前（项目规则 20，现有已遵守，改动保持）。
- **测试约定**：项目**无 vitest/jest**。纯函数测试用 `npx tsx <file>.test.ts`（自定义 `assertEqual`，不依赖 `node:assert`，因 tsconfig 未启用 Node 类型）。hook/UI 组件/tauri API 调用无法在 Node 单测，靠 `npm run build`（tsc 类型检查）+ 手动验证。
- **复用**（规则 9）：平台判断复用 `shortcutRecorder` 的 `navigator.platform` 模式；通知检测/请求抽成 `lib/notification.ts` 供 hook 与 Settings 共用。
- **提交**：遵循仓库约定「仅当用户要求时执行 `git commit`」。计划中的 commit 步骤为可选检查点；执行时若用户未要求提交，跳过 commit 步骤、保留工作区改动即可。

---

## File Structure

| 文件 | 职责 | 动作 |
|------|------|------|
| `web/src/lib/platform.ts` | `isMacos()` 平台判断（复用 navigator.platform） | 新增 |
| `web/src/lib/platform.test.ts` | `isMacos` 纯函数测试 | 新增 |
| `web/src/lib/notification.ts` | `checkNotificationGranted()` + `requestNotificationPermission()`（封装 tauri notification JS API） | 新增 |
| `web/src/lib/types.ts` | `PermissionsStatus` 加 `notification`、`PermissionType` 加 `'notification'` | 修改 |
| `web/src/lib/icons.tsx` | 新增 `BellIcon`（16x16 stroke-based 铃铛） | 修改 |
| `web/src/lib/permissionEntries.tsx` | `mapPermissions` 加第 4 条 notification | 修改 |
| `web/src/lib/permissionEntries.test.ts` | `mapPermissions` 纯函数测试（4 条 + notification） | 新增 |
| `web/src/hooks/usePermissions.ts` | 轮询合并通知状态、`requestMissing` 分支请求 | 修改 |
| `web/src/pages/Settings/Settings.tsx` | `handleRequestAccess` 对 notification 走 JS API | 修改 |
| `web/src/i18n/locales/zh/welcome.json` | `permission.notification.*` 中文文案 | 修改 |
| `web/src/i18n/locales/en/welcome.json` | `permission.notification.*` 英文文案 | 修改 |
| `web/CLAUDE.md` | 权限流程（3→4）+ `usePermissions` 描述更新 | 修改 |

任务依赖：Task 1 → Task 2（用 isMacos）→ Task 3（types/icon/i18n）→ Task 4（mapPermissions 用 BellIcon+types）→ Task 5（hook 用 notification+types）→ Task 6（Settings 用 notification）→ Task 7（文档）。

---

### Task 1: `lib/platform.ts` — `isMacos()` 平台判断（TDD）

**Files:**
- Create: `web/src/lib/platform.ts`
- Test: `web/src/lib/platform.test.ts`

**Interfaces:**
- Produces: `isMacos(platform?: string): boolean` —— 缺省读 `globalThis.navigator?.platform`，含 `'mac'` 即 true。`lib/notification.ts`（Task 2）以 `isMacos()` 无参调用。

- [ ] **Step 1: Write the failing test**

Create `web/src/lib/platform.test.ts`:

```ts
import { isMacos } from './platform';

function assertEqual(actual: unknown, expected: unknown, msg?: string): void {
  if (!Object.is(actual, expected)) {
    throw new Error(`${msg ?? ''} Expected ${String(expected)}, got ${String(actual)}`);
  }
}

assertEqual(isMacos('MacIntel'), true, 'MacIntel is mac');
assertEqual(isMacos('Macintosh'), true, 'Macintosh is mac');
assertEqual(isMacos('Win32'), false, 'Win32 not mac');
assertEqual(isMacos('Linux x86_64'), false, 'Linux not mac');
assertEqual(isMacos(''), false, 'empty not mac');

console.log('platform.test.ts passed');
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd web && npx tsx src/lib/platform.test.ts`
Expected: FAIL —— `Cannot find module './platform'`（文件尚未创建）。

- [ ] **Step 3: Write minimal implementation**

Create `web/src/lib/platform.ts`:

```ts
/**
 * platform - 运行平台判断（复用 navigator.platform）
 *
 * Business Logic（为什么需要这个模块）:
 *   macOS 通知权限引导（lib/notification.ts、usePermissions）需区分是否在 macOS：非 macOS
 *   视为通知已授权、不发 tauri notification JS API 调用。复用 shortcutRecorder 已采用的
 *   navigator.platform 判断模式，避免重复造平台检测逻辑（项目规则 9 复用）。
 *
 * Code Logic（这个模块做什么）:
 *   isMacos(platform?) 读取 navigator.platform（可注入便于单测），小写后包含 'mac' 即 true。
 */

/**
 * 判断当前是否运行在 macOS
 *
 * Business Logic: 通知权限等 macOS 专属引导需据此跳过非 macOS。
 * Code Logic: navigator.platform 含 'mac'（MacIntel/Macintosh）即 true；接受可选 platform
 *   参数便于单测注入，缺省读 globalThis.navigator?.platform。
 *
 * @param platform 平台字符串，缺省读 globalThis.navigator.platform
 * @returns 是否 macOS
 */
export function isMacos(platform: string = globalThis.navigator?.platform ?? ''): boolean {
  return platform.toLowerCase().includes('mac');
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd web && npx tsx src/lib/platform.test.ts`
Expected: PASS（stdout 打印 `platform.test.ts passed`）。

- [ ] **Step 5: Commit（可选检查点，遵循 Global Constraints 提交约定）**

```bash
git add web/src/lib/platform.ts web/src/lib/platform.test.ts
git commit -m "feat: 新增 lib/platform.ts isMacos 平台判断"
```

---

### Task 2: `lib/notification.ts` — 通知权限检测/请求（tsc 验证）

**Files:**
- Create: `web/src/lib/notification.ts`

**Interfaces:**
- Consumes: `isMacos()` from `./platform`（Task 1）；`isPermissionGranted`、`requestPermission` from `@tauri-apps/plugin-notification`（已装）。
- Produces:
  - `checkNotificationGranted(): Promise<boolean>` —— macOS 调 `isPermissionGranted()`，非 macOS/异常返回 `true`。
  - `requestNotificationPermission(): Promise<void>` —— macOS 调 `requestPermission()`，非 macOS/异常静默 no-op。
  - 被 Task 5（`usePermissions`）与 Task 6（`Settings`）共用。

> 本任务无法 Node 单测（依赖 tauri runtime 的 JS API）。`isMacos` 分支已由 Task 1 覆盖；本任务靠 `npm run build`（tsc）验证类型与 import 正确，macOS 行为留待手动验证（Task 5/6）。

- [ ] **Step 1: Write the implementation**

Create `web/src/lib/notification.ts`:

```ts
/**
 * notification - 通知权限检测/请求（@tauri-apps/plugin-notification 封装）
 *
 * Business Logic（为什么需要这个模块）:
 *   cc-partner 通过系统通知推送健康提醒（久坐/喝水）。macOS 需用户授权「通知」权限，
 *   欢迎页/设置页的第 4 个权限引导需检测与请求它。通知权限不属于 TCC（不走 Rust FFI），
 *   由 tauri-plugin-notification 的 JS API 管理，故独立成模块供 usePermissions hook 与
 *   Settings 页共用（项目规则 9 复用，避免两处各写一份）。
 *
 * Code Logic（这个模块做什么）:
 *   - checkNotificationGranted(): macOS 调 isPermissionGranted()，非 macOS 视为已授权
 *   - requestNotificationPermission(): macOS 调 requestPermission()，非 macOS no-op
 *   两者探测/请求失败均保守降级（视为已授权 / 静默），不阻断主流程（通知是可选功能）。
 */

import { isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification';
import { isMacos } from './platform';

/**
 * 查询通知授权状态
 *
 * Business Logic: usePermissions 轮询时合并进统一权限视图，供权限卡片显示授权状态。
 * Code Logic: 非 macOS 一律返回 true（不引导）；macOS 调 isPermissionGranted()，
 *   异常保守返回 true（探测失败不阻断）。
 *
 * @returns 是否已授权发送通知
 */
export async function checkNotificationGranted(): Promise<boolean> {
  if (!isMacos()) return true;
  try {
    return await isPermissionGranted();
  } catch {
    return true;
  }
}

/**
 * 请求通知权限
 *
 * Business Logic: 用户在欢迎页/设置页点「去设置」时触发，弹系统通知授权框。
 * Code Logic: 非 macOS no-op；macOS 调 requestPermission()（返回 granted/denied/default）。
 *   授权状态由 usePermissions 轮询反映，此处不直接写 state（保持单一数据源）。
 */
export async function requestNotificationPermission(): Promise<void> {
  if (!isMacos()) return;
  try {
    await requestPermission();
  } catch {
    // 请求失败静默，轮询反映真实状态
  }
}
```

- [ ] **Step 2: Verify tsc compiles**

Run: `cd web && npm run build`
Expected: PASS（tsc 类型检查 + vite 构建通过；`isPermissionGranted`/`requestPermission` 来自已装的 `@tauri-apps/plugin-notification`）。

- [ ] **Step 3: Commit（可选检查点）**

```bash
git add web/src/lib/notification.ts
git commit -m "feat: 新增 lib/notification.ts 通知权限检测/请求"
```

---

### Task 3: 类型扩展 + BellIcon + i18n 文案（tsc 验证）

**Files:**
- Modify: `web/src/lib/types.ts`（`PermissionsStatus` / `PermissionType`）
- Modify: `web/src/lib/icons.tsx`（新增 `BellIcon`）
- Modify: `web/src/i18n/locales/zh/welcome.json`
- Modify: `web/src/i18n/locales/en/welcome.json`

**Interfaces:**
- Produces:
  - `PermissionsStatus` 增加 `notification: { granted: boolean }`。
  - `PermissionType` 增加 `'notification'`。
  - `BellIcon`（`IconProps`，16x16 stroke-based）。
  - i18n key `permission.notification.title` / `permission.notification.description`（en + zh）。
  - 被 Task 4（mapPermissions）/Task 5（hook）依赖。

- [ ] **Step 1: Extend types**

Modify `web/src/lib/types.ts` —— 把现有的：

```ts
export interface PermissionsStatus {
  screenCapture: { granted: boolean };
  inputMonitoring: { granted: boolean };
  accessibility: { granted: boolean };
}

export type PermissionType = 'screenCapture' | 'inputMonitoring' | 'accessibility';
```

改为：

```ts
export interface PermissionsStatus {
  screenCapture: { granted: boolean };
  inputMonitoring: { granted: boolean };
  accessibility: { granted: boolean };
  /** 通知权限（前端 JS API 检测合并；后端 check_permissions 不含此字段） */
  notification: { granted: boolean };
}

export type PermissionType = 'screenCapture' | 'inputMonitoring' | 'accessibility' | 'notification';
```

- [ ] **Step 2: Add BellIcon**

Modify `web/src/lib/icons.tsx` —— 在文件末尾（最后一个 `export const` 图标之后）追加：

```tsx
export const BellIcon = ({ size, ...rest }: IconProps) => (
  <svg {...baseProps(size)} {...rest}>
    <path d="M8 1.5v1" />
    <path d="M4 6a4 4 0 0 1 8 0c0 3 1.5 4.5 2 5.5H2c.5-1 2-2.5 2-5.5Z" />
    <path d="M6.8 13.2a1.3 1.3 0 0 0 2.4 0" />
  </svg>
);
```

（沿用文件顶部 `IconProps` 与 `baseProps`；16x16 viewBox、stroke `currentColor`、strokeWidth 1.6，与现有图标一致。）

- [ ] **Step 3: Add zh i18n**

Modify `web/src/i18n/locales/zh/welcome.json` —— 把 `permission` 对象的 `inputMonitoring` 块改为：

```json
    "inputMonitoring": {
      "title": "输入监控",
      "description": "健康提醒检测键盘活动需要"
    },
    "notification": {
      "title": "通知",
      "description": "接收健康提醒等系统通知"
    }
```

（即在 `inputMonitoring` 块的 `}` 后加逗号，再追加 `notification` 块。）

- [ ] **Step 4: Add en i18n**

Modify `web/src/i18n/locales/en/welcome.json` —— 把 `permission` 对象的 `inputMonitoring` 块改为：

```json
    "inputMonitoring": {
      "title": "Input Monitoring",
      "description": "Health reminder detects keyboard activity"
    },
    "notification": {
      "title": "Notifications",
      "description": "Receive system notifications like health reminders"
    }
```

- [ ] **Step 5: Verify build (tsc + i18n key 校验)**

Run: `cd web && npm run build`
Expected: PASS。tsc 会校验 i18n key（react-i18next 类型扩展，拼错即报错）；新增 `notification` key 被 Task 4 的 `t('permission.notification.title')` 引用前必须存在。

- [ ] **Step 6: Commit（可选检查点）**

```bash
git add web/src/lib/types.ts web/src/lib/icons.tsx web/src/i18n/locales/zh/welcome.json web/src/i18n/locales/en/welcome.json
git commit -m "feat: PermissionsStatus 加 notification 字段 + BellIcon + i18n 文案"
```

---

### Task 4: `mapPermissions` 加第 4 条 notification（TDD）

**Files:**
- Modify: `web/src/lib/permissionEntries.tsx`
- Test: `web/src/lib/permissionEntries.test.ts`

**Interfaces:**
- Consumes: `BellIcon`（Task 3）；`PermissionsStatus`（Task 3，含 `notification`）；`t('permission.notification.title'|'.description')`（Task 3 i18n）。
- Produces: `mapPermissions` 返回 4 条 `PermissionEntry`，顺序 screenCapture → accessibility → inputMonitoring → **notification**，第 4 条 `granted = status.notification.granted`。

- [ ] **Step 1: Write the failing test**

Create `web/src/lib/permissionEntries.test.ts`:

```ts
import { mapPermissions } from './permissionEntries';
import type { PermissionsStatus } from './types';

function assertEqual(actual: unknown, expected: unknown, msg?: string): void {
  if (!Object.is(actual, expected)) {
    throw new Error(`${msg ?? ''} Expected ${String(expected)}, got ${String(actual)}`);
  }
}

// mock t：直接回传 key，便于断言文案 key（mapPermissions 内部 t('permission.notification.title') 等）
const t = ((key: string) => key) as never;

const status: PermissionsStatus = {
  screenCapture: { granted: true },
  accessibility: { granted: true },
  inputMonitoring: { granted: false },
  notification: { granted: false },
};

const entries = mapPermissions(status, t);

assertEqual(entries.length, 4, '应返回 4 条权限');
assertEqual(entries[0].id, 'screenCapture');
assertEqual(entries[1].id, 'accessibility');
assertEqual(entries[2].id, 'inputMonitoring');
assertEqual(entries[3].id, 'notification', 'notification 应为第 4 条');
assertEqual(entries[3].granted, false, 'notification granted 镜像 status');
assertEqual(entries[3].title, 'permission.notification.title', 'notification 标题文案 key');

console.log('permissionEntries.test.ts passed');
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd web && npx tsx src/lib/permissionEntries.test.ts`
Expected: FAIL —— `entries.length` 为 3（notification 条目尚未加），断言 "应返回 4 条权限" 失败。

- [ ] **Step 3: Write minimal implementation**

Modify `web/src/lib/permissionEntries.tsx`：

1. 在 import 行加 `BellIcon`。现有：

```tsx
import { HealthIcon, InfoIcon, KeyboardIcon } from '@/lib/icons';
```

改为：

```tsx
import { BellIcon, HealthIcon, InfoIcon, KeyboardIcon } from '@/lib/icons';
```

2. 在 `mapPermissions` 返回数组的 `inputMonitoring` 条目后、闭合 `]` 前追加 notification 条目。现有数组末尾：

```tsx
    {
      id: 'inputMonitoring',
      icon: <KeyboardIcon />,
      title: t('permission.inputMonitoring.title'),
      description: t('permission.inputMonitoring.description'),
      granted: status.inputMonitoring.granted,
    },
  ];
```

改为：

```tsx
    {
      id: 'inputMonitoring',
      icon: <KeyboardIcon />,
      title: t('permission.inputMonitoring.title'),
      description: t('permission.inputMonitoring.description'),
      granted: status.inputMonitoring.granted,
    },
    {
      id: 'notification',
      icon: <BellIcon />,
      title: t('permission.notification.title'),
      description: t('permission.notification.description'),
      granted: status.notification.granted,
    },
  ];
```

3. 更新文件顶部 docstring「Code Logic」一行（把「顺序固定为屏幕录制 → 输入监控」改为「屏幕录制 → 辅助功能 → 输入监控 → 通知」），并补充 notification 真实消费者说明，保持注释准确（项目规则 5/注释规范）。即把：

```tsx
 *   `mapPermissions(status, t)` 接收 welcome ns 的翻译函数，返回 PermissionEntry[]，
 *   顺序固定为屏幕录制 → 输入监控，文案/图标与 Python 版权限项一致。
```

改为：

```tsx
 *   `mapPermissions(status, t)` 接收 welcome ns 的翻译函数，返回 PermissionEntry[]，
 *   顺序固定为屏幕录制 → 辅助功能 → 输入监控 → 通知。通知权限由前端 JS API 检测
 *   （lib/notification.ts），非 TCC；仅 macOS 引导，非 macOS 视为已授权。
```

并更新该函数 docstring「Business Logic（三条权限的真实消费者）」标题为「四条权限的真实消费者」，追加一行：

```tsx
 *   - 通知：系统通知（健康提醒久坐/喝水），由 @tauri-apps/plugin-notification 发送，需用户授权
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd web && npx tsx src/lib/permissionEntries.test.ts`
Expected: PASS（stdout 打印 `permissionEntries.test.ts passed`）。

- [ ] **Step 5: Verify full build**

Run: `cd web && npm run build`
Expected: PASS（tsc 校验 BellIcon import、i18n key、types）。

- [ ] **Step 6: Commit（可选检查点）**

```bash
git add web/src/lib/permissionEntries.tsx web/src/lib/permissionEntries.test.ts
git commit -m "feat: mapPermissions 新增通知权限第 4 条 + 单测"
```

---

### Task 5: `usePermissions` 合并通知状态 + 分支请求（tsc + 手动）

**Files:**
- Modify: `web/src/hooks/usePermissions.ts`

**Interfaces:**
- Consumes: `checkNotificationGranted`、`requestNotificationPermission` from `@/lib/notification`（Task 2）；`PermissionsStatus`（Task 3，含 notification）。
- Produces: hook 的 `status` 现含 `notification.granted`（合并自 `checkNotificationGranted()`）；`requestMissing()` 对 notification 走 `requestNotificationPermission()`、其余走 `configApi.requestPermission`。`allGranted` 与轮询停止判定**不变**（只看 3 个 TCC）。

- [ ] **Step 1: Add import**

Modify `web/src/hooks/usePermissions.ts`。现有 import 段：

```ts
import { useCallback, useEffect, useRef, useState } from 'react';
import { configApi } from '@/api/config';
import type { PermissionType, PermissionsStatus } from '@/lib/types';
```

加 `lib/notification` import，并**移除重构后不再使用的 `PermissionType`**（新 `requestMissing` 不再用 `PermissionType` 类型标注，`'screenCapture'` 等 string literal 直接传给 `configApi.requestPermission` 即可；保留未使用 import 会触发 `noUnusedLocals` 报错）：

```ts
import { useCallback, useEffect, useRef, useState } from 'react';
import { configApi } from '@/api/config';
import { checkNotificationGranted, requestNotificationPermission } from '@/lib/notification';
import type { PermissionsStatus } from '@/lib/types';
```

- [ ] **Step 2: Merge notification status in refresh**

现有 `refresh`：

```ts
  const refresh = useCallback(async () => {
    try {
      const s = await configApi.permissions();
      setStatus(s);
    } catch {
      // 接口失败保持当前状态，下轮重试
    }
  }, []);
```

改为（并发拉 TCC + 通知状态，合并）：

```ts
  const refresh = useCallback(async () => {
    try {
      const [tcc, notifyGranted] = await Promise.all([
        configApi.permissions(),
        checkNotificationGranted(),
      ]);
      setStatus({ ...tcc, notification: { granted: notifyGranted } });
    } catch {
      // 接口失败保持当前状态，下轮重试
    }
  }, []);
```

- [ ] **Step 3: Branch requestMissing by type**

现有 `requestMissing`：

```ts
  const requestMissing = useCallback(async () => {
    const current = statusRef.current;
    const types: PermissionType[] = [];
    if (current && !current.screenCapture.granted) types.push('screenCapture');
    if (current && !current.accessibility.granted) types.push('accessibility');
    if (current && !current.inputMonitoring.granted) types.push('inputMonitoring');
    if (types.length === 0) return;
    await Promise.all(types.map((t) => configApi.requestPermission(t)));
    await refresh();
  }, [refresh]);
```

改为（notification 单独走 JS API，TCC 走 configApi）：

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

> `allGranted` 与轮询 `done` 判定**保持只看 3 个 TCC 权限**（不阻塞「继续使用」），**不改动**。

- [ ] **Step 4: Update docstring**

更新文件顶部「Code Logic」注释：`requestMissing()` 一行改为「对所有未授权权限调用授权（TCC 走 configApi.requestPermission、通知走 requestNotificationPermission），随后立即刷新」。保持注释与实现一致（项目注释规范）。

- [ ] **Step 5: Verify build**

Run: `cd web && npm run build`
Expected: PASS。

- [ ] **Step 6: Manual verification（需 macOS，用户介入）**

Run: `cd web && ./node_modules/.bin/tauri dev`
验证点：
1. 首次未授权时跳到 `/welcome`，权限列表显示**第 4 张「通知」卡片**（铃铛图标 + 「去设置」按钮）。
2. 点「去设置」→ macOS 弹出系统通知授权对话框（「cc-partner 想要给你发送通知」）。
3. 授权后约 2s 内通知卡片状态刷新为「已授权」。
4. 「继续使用」按钮在 3 个 TCC 权限全授权后即可点击（通知未授权**不阻塞**）。
5. 进入「设置 → 常规 → 权限管理」同样看到 4 张卡片。
6. 切换中/英文，通知卡片标题/说明随语言切换。

- [ ] **Step 7: Commit（可选检查点）**

```bash
git add web/src/hooks/usePermissions.ts
git commit -m "feat: usePermissions 合并通知权限状态并分支请求"
```

---

### Task 6: Settings `handleRequestAccess` 区分 notification（tsc + 手动）

**Files:**
- Modify: `web/src/pages/Settings/Settings.tsx`

**Interfaces:**
- Consumes: `requestNotificationPermission` from `@/lib/notification`（Task 2）；`PermissionType`（Task 3）。
- 作用：设置页权限 Card 单项「去设置」对 notification 走 JS API，其余走后端命令。

- [ ] **Step 1: Add import**

现有 import（Settings.tsx 顶部）：

```ts
import { configApi } from '@/api/config';
```

在其后加：

```ts
import { requestNotificationPermission } from '@/lib/notification';
```

- [ ] **Step 2: Branch handleRequestAccess by type**

现有 `handleRequestAccess`（约 195-205 行）：

```ts
  const handleRequestAccess = useCallback(
    async (type: PermissionType) => {
      try {
        await configApi.requestPermission(type);
        await refreshPermissions();
      } catch {
        // 请求失败静默，轮询会持续反映真实状态
      }
    },
    [refreshPermissions],
  );
```

改为：

```ts
  const handleRequestAccess = useCallback(
    async (type: PermissionType) => {
      try {
        if (type === 'notification') {
          await requestNotificationPermission();
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

> JSX（权限管理 Card 的 `mapPermissions(...).map(...)`）**无需改动**：notification 卡片随 mapPermissions 自动出现，`onRequestAccess={() => void handleRequestAccess(p.id as PermissionType)}` 对 notification 传入 `'notification'`，命中新分支。

- [ ] **Step 3: Update docstring**

`handleRequestAccess` 的 docstring「@param type 权限类型 screenCapture / inputMonitoring」改为「权限类型 screenCapture / accessibility / inputMonitoring / notification（notification 走前端 JS API）」。

- [ ] **Step 4: Verify build**

Run: `cd web && npm run build`
Expected: PASS。

- [ ] **Step 5: Manual verification（需 macOS，用户介入）**

Run: `cd web && ./node_modules/.bin/tauri dev`
验证点：进入「设置 → 常规 → 权限管理」，通知卡片点「去设置」弹出系统通知授权框；授权后刷新为「已授权」；其余 3 个权限「去设置」行为不变（打开系统设置隐私面板）。

- [ ] **Step 6: Commit（可选检查点）**

```bash
git add web/src/pages/Settings/Settings.tsx
git commit -m "feat: Settings 权限请求区分 notification(前端 JS API)"
```

---

### Task 7: 更新 `web/CLAUDE.md`（文档）

**Files:**
- Modify: `web/CLAUDE.md`

- [ ] **Step 1: 更新「macOS 权限流程」节**

在 `web/CLAUDE.md` 的「macOS 权限流程」段，把开头的「三条权限的真实消费者」描述更新为四条，并追加通知权限说明。定位该段（约第 33 行）开头的描述：

```
- **macOS 权限流程**: 三条权限的真实消费者——屏幕录制（区域截图）、辅助功能（健康提醒活动窗口标题采样 active-win-pos-rs）、输入监控（健康提醒键鼠采样 device_query）；
```

改为：

```
- **macOS 权限流程**: 四条权限的真实消费者——屏幕录制（区域截图）、辅助功能（健康提醒活动窗口标题采样 active-win-pos-rs）、输入监控（健康提醒键鼠采样 device_query）、通知（系统通知，健康提醒久坐/喝水，@tauri-apps/plugin-notification）；
```

并在该段末尾（「设置页「权限管理」Card……提供随时查看状态 / 单项重新授权的常驻入口（**后端命令 M7 实现**）」之后）追加一句：

```
通知权限为第 4 条，由前端 JS API 检测/请求（`lib/notification.ts` 的 `checkNotificationGranted`/`requestNotificationPermission`，macOS 调 `isPermissionGranted`/`requestPermission`，非 macOS 视为已授权），`usePermissions` 轮询合并进 `PermissionsStatus.notification`；**不阻塞「继续使用」**（`allGranted` 仍只看 3 个 TCC 权限），仅 macOS 引导。
```

- [ ] **Step 2: 更新「自定义 Hook」节 usePermissions 描述**

定位「自定义 Hook」节中 `usePermissions` 一行：

```
`usePermissions`（macOS 权限状态轮询 + 请求授权，导出 `PERMISSION_ONBOARDED_KEY` 供 OnboardingGuard/Welcome 共享）
```

改为：

```
`usePermissions`（macOS 权限状态轮询 + 请求授权：3 个 TCC 权限走 `configApi`、通知权限走 `lib/notification.ts` JS API 并合并进 `PermissionsStatus.notification`；`allGranted` 只看 3 个 TCC 权限不阻塞「继续使用」；导出 `PERMISSION_ONBOARDED_KEY` 供 OnboardingGuard/Welcome 共享）
```

- [ ] **Step 3: Commit（可选检查点）**

```bash
git add web/CLAUDE.md
git commit -m "docs: CLAUDE.md 权限流程更新为四权限(新增通知)"
```

---

## 完成标准（Definition of Done）

- [ ] `cd web && npm run build` 通过（tsc 类型检查 + vite 构建）。
- [ ] `cd web && npx tsx src/lib/platform.test.ts && npx tsx src/lib/permissionEntries.test.ts` 两个测试均 PASS。
- [ ] macOS 上 `tauri dev` 手动验证：Welcome 与 Settings 显示第 4 张通知卡片，点「去设置」弹授权框，授权后刷新，通知未授权不阻塞「继续使用」。
- [ ] `web/CLAUDE.md` 已更新（3→4 权限）。
- [ ] 后端 `src-tauri/` 无任何改动（`git diff --stat src-tauri/` 为空）。

## 未纳入范围（相关功能，规则 19）

通知**消费侧**（`HealthReminderListener` 监听 `health:reminder` → `sendNotification` 弹系统通知）尚未实现。本计划只做权限引导。是否补上消费侧，待用户确认后另开计划。
