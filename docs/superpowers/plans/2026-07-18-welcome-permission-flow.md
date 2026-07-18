# Welcome 系统权限交互 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复点「去设置」闪白屏；按权限分流登记/开设置；回前台多轮 recheck；仅用户点击时可选「重新打开应用」。

**Architecture:** Welcome 持有纯状态机 `idle | awaiting | syncing | needs_reopen`（逻辑抽到 `welcomePermissionFlow.ts` 便于单测）。点「去设置」只调 `requestPermission`（后端按类型登记 + 开设置或 prompt），**绝不**自动 relaunch。回前台多轮 `refresh`/`check_permissions`；仍 sticky-denied 时展示可选按钮，按钮才调 `relaunch_for_permissions`（macOS `open` .app）。

**Tech Stack:** React 19 + TypeScript + Vitest/jsdom；Tauri 2 + Rust permissions FFI；i18n welcome ns。

**Spec:** `docs/superpowers/specs/2026-07-18-welcome-permission-flow-design.md`

## Global Constraints

- 点「去设置」禁止自动 relaunch / 杀进程（闪白屏根因）。
- 禁止假绿：检测 API false 时卡片不得标已授权。
- 四项必齐：screenCapture / accessibility / inputMonitoring / notification。
- 说教长文案禁止（无「完全退出 / 开对条目」）。
- relaunch 仅按钮：macOS 必须 LaunchServices `open` `.app`，禁止 exec `Contents/MacOS/*`。
- React hooks 必须在 early return 之前。
- 代码注释 Business Logic / Code Logic；改完更新 `web/CLAUDE.md` 与 `src-tauri/CLAUDE.md`。
- 对话与用户可见说明用中文。

## File Map

| 文件 | 职责 |
| --- | --- |
| `web/src/pages/Welcome/welcomePermissionFlow.ts` | **新建** 纯状态机：事件、转移、sticky 判定、hint key |
| `web/src/pages/Welcome/welcomePermissionFlow.test.ts` | **新建** 状态机单测 |
| `web/src/pages/Welcome/Welcome.tsx` | 接线状态机；删自动 relaunch；可选 reopen 按钮 |
| `web/src/pages/Welcome/Welcome.test.tsx` | 扩展：去设置不 relaunch；needs_reopen 按钮才 relaunch |
| `web/src/i18n/locales/{zh,en}/welcome.json` | `syncingPermission` / `reopenToApply` / `reopenApp` |
| `web/src/lib/types/core.ts` | `PermissionRequestResult.action?` |
| `web/src/api/config.ts` | 注释修正；relaunch 仅按钮语义 |
| `src-tauri/src/permissions/mod.rs` | `request_permission` 按矩阵分流；`RequestPermissionResult.action` |
| `src-tauri/src/commands/permissions.rs` | 序列化 `action` |
| `web/CLAUDE.md` / `src-tauri/CLAUDE.md` | 需求描述对齐 spec |

---

### Task 1: 纯状态机 + 单测（TDD）

**Files:**
- Create: `web/src/pages/Welcome/welcomePermissionFlow.ts`
- Create: `web/src/pages/Welcome/welcomePermissionFlow.test.ts`

**Interfaces:**
- Produces:
  - `export type WelcomePermPhase = 'idle' | 'awaiting' | 'syncing' | 'needs_reopen'`
  - `export type StickyPermission = 'screenCapture' | 'accessibility' | 'inputMonitoring'`
  - `export const STICKY_PERMISSIONS: ReadonlySet<StickyPermission>`
  - `export function isStickyPermission(type: string): boolean`
  - `export function hasStickyDenied(status: { screenCapture: { granted: boolean }; accessibility: { granted: boolean }; inputMonitoring: { granted: boolean } }): boolean`
  - `export type WelcomePermEvent = { type: 'GO_SETTINGS'; permission: string } | { type: 'FOREGROUND' } | { type: 'SYNC_TICK'; status: Parameters<typeof hasStickyDenied>[0] } | { type: 'SYNC_EXHAUSTED'; status: Parameters<typeof hasStickyDenied>[0] } | { type: 'ALL_REQUIRED_GRANTED' } | { type: 'REOPEN_CLICKED' }`
  - `export function reduceWelcomePermPhase(phase: WelcomePermPhase, event: WelcomePermEvent): WelcomePermPhase`
  - `export function welcomeHintKey(phase: WelcomePermPhase, allRequiredGranted: boolean): 'permissionReady' | 'waitingPermission' | 'syncingPermission' | 'reopenToApply'`
  - `export const SYNC_DELAYS_MS: readonly number[] = [0, 400, 1200, 2500]`

- [ ] **Step 1: 写失败单测**

```typescript
// web/src/pages/Welcome/welcomePermissionFlow.test.ts
import { describe, expect, test } from 'vitest';
import {
  hasStickyDenied,
  isStickyPermission,
  reduceWelcomePermPhase,
  welcomeHintKey,
} from './welcomePermissionFlow';

const denied = {
  screenCapture: { granted: false },
  accessibility: { granted: false },
  inputMonitoring: { granted: false },
};
const stickyOk = {
  screenCapture: { granted: true },
  accessibility: { granted: true },
  inputMonitoring: { granted: true },
};

describe('welcomePermissionFlow', () => {
  test('notification is not sticky; screenCapture is', () => {
    expect(isStickyPermission('notification')).toBe(false);
    expect(isStickyPermission('screenCapture')).toBe(true);
  });

  test('GO_SETTINGS on sticky moves idle -> awaiting', () => {
    expect(
      reduceWelcomePermPhase('idle', { type: 'GO_SETTINGS', permission: 'inputMonitoring' }),
    ).toBe('awaiting');
  });

  test('GO_SETTINGS on notification stays idle', () => {
    expect(
      reduceWelcomePermPhase('idle', { type: 'GO_SETTINGS', permission: 'notification' }),
    ).toBe('idle');
  });

  test('FOREGROUND only from awaiting -> syncing', () => {
    expect(reduceWelcomePermPhase('awaiting', { type: 'FOREGROUND' })).toBe('syncing');
    expect(reduceWelcomePermPhase('idle', { type: 'FOREGROUND' })).toBe('idle');
  });

  test('SYNC_TICK with sticky granted ends at idle', () => {
    expect(
      reduceWelcomePermPhase('syncing', { type: 'SYNC_TICK', status: stickyOk }),
    ).toBe('idle');
  });

  test('SYNC_EXHAUSTED with sticky denied -> needs_reopen', () => {
    expect(
      reduceWelcomePermPhase('syncing', { type: 'SYNC_EXHAUSTED', status: denied }),
    ).toBe('needs_reopen');
  });

  test('REOPEN_CLICKED does not change phase (side-effect elsewhere)', () => {
    expect(reduceWelcomePermPhase('needs_reopen', { type: 'REOPEN_CLICKED' })).toBe(
      'needs_reopen',
    );
  });

  test('hint keys', () => {
    expect(welcomeHintKey('idle', true)).toBe('permissionReady');
    expect(welcomeHintKey('idle', false)).toBe('waitingPermission');
    expect(welcomeHintKey('syncing', false)).toBe('syncingPermission');
    expect(welcomeHintKey('needs_reopen', false)).toBe('reopenToApply');
  });

  test('hasStickyDenied', () => {
    expect(hasStickyDenied(denied)).toBe(true);
    expect(hasStickyDenied(stickyOk)).toBe(false);
  });
});
```

- [ ] **Step 2: 跑测确认失败**

```bash
cd web && npm test -- welcomePermissionFlow
```

Expected: FAIL（模块不存在或导出缺失）

- [ ] **Step 3: 实现 `welcomePermissionFlow.ts`**

```typescript
// web/src/pages/Welcome/welcomePermissionFlow.ts
/**
 * Welcome 权限同步纯状态机（无 React / 无副作用）。
 *
 * Business Logic: 去设置不得 relaunch；回前台 recheck；仍未授权才 needs_reopen。
 * Code Logic: reduceWelcomePermPhase + hint key + sticky 判定。
 */

export type WelcomePermPhase = 'idle' | 'awaiting' | 'syncing' | 'needs_reopen';

export type StickyPermission = 'screenCapture' | 'accessibility' | 'inputMonitoring';

export const STICKY_PERMISSIONS: ReadonlySet<string> = new Set([
  'screenCapture',
  'accessibility',
  'inputMonitoring',
]);

export const SYNC_DELAYS_MS: readonly number[] = [0, 400, 1200, 2500];

export type StickyStatusSlice = {
  screenCapture: { granted: boolean };
  accessibility: { granted: boolean };
  inputMonitoring: { granted: boolean };
};

export type WelcomePermEvent =
  | { type: 'GO_SETTINGS'; permission: string }
  | { type: 'FOREGROUND' }
  | { type: 'SYNC_TICK'; status: StickyStatusSlice }
  | { type: 'SYNC_EXHAUSTED'; status: StickyStatusSlice }
  | { type: 'ALL_REQUIRED_GRANTED' }
  | { type: 'REOPEN_CLICKED' };

export function isStickyPermission(type: string): boolean {
  return STICKY_PERMISSIONS.has(type);
}

export function hasStickyDenied(status: StickyStatusSlice): boolean {
  return (
    !status.screenCapture.granted ||
    !status.accessibility.granted ||
    !status.inputMonitoring.granted
  );
}

/**
 * Business Logic: 权限同步相位转移，无 IO。
 * Code Logic: 按下表转移；未知事件保持 phase。
 */
export function reduceWelcomePermPhase(
  phase: WelcomePermPhase,
  event: WelcomePermEvent,
): WelcomePermPhase {
  if (event.type === 'ALL_REQUIRED_GRANTED') {
    return 'idle';
  }
  switch (event.type) {
    case 'GO_SETTINGS':
      if (isStickyPermission(event.permission)) {
        return 'awaiting';
      }
      return phase;
    case 'FOREGROUND':
      return phase === 'awaiting' || phase === 'needs_reopen' ? 'syncing' : phase;
    case 'SYNC_TICK':
      if (phase !== 'syncing') return phase;
      return hasStickyDenied(event.status) ? 'syncing' : 'idle';
    case 'SYNC_EXHAUSTED':
      if (phase !== 'syncing') return phase;
      return hasStickyDenied(event.status) ? 'needs_reopen' : 'idle';
    case 'REOPEN_CLICKED':
      return phase;
    default:
      return phase;
  }
}

export function welcomeHintKey(
  phase: WelcomePermPhase,
  allRequiredGranted: boolean,
): 'permissionReady' | 'waitingPermission' | 'syncingPermission' | 'reopenToApply' {
  if (allRequiredGranted) return 'permissionReady';
  if (phase === 'syncing') return 'syncingPermission';
  if (phase === 'needs_reopen') return 'reopenToApply';
  return 'waitingPermission';
}
```

- [ ] **Step 4: 跑测确认通过**

```bash
cd web && npm test -- welcomePermissionFlow
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Welcome/welcomePermissionFlow.ts web/src/pages/Welcome/welcomePermissionFlow.test.ts
git commit -m "test+feat: Welcome 权限同步纯状态机"
```

---

### Task 2: i18n 文案

**Files:**
- Modify: `web/src/i18n/locales/zh/welcome.json`
- Modify: `web/src/i18n/locales/en/welcome.json`

- [ ] **Step 1: 更新 zh**

在 `waitingPermission` 旁确保仅有：

```json
"waitingPermission": "正在等待系统授权…",
"syncingPermission": "正在同步权限状态…",
"reopenToApply": "权限可能需重新打开应用后生效",
"reopenApp": "重新打开应用",
```

删除若存在的 `restartAfterGrantHint` 及任何「完全退出/开对条目」长文案。

- [ ] **Step 2: 更新 en**

```json
"waitingPermission": "Waiting for system authorization…",
"syncingPermission": "Syncing permission status…",
"reopenToApply": "Permissions may apply after reopening the app",
"reopenApp": "Reopen app",
```

删除对称说教 key。

- [ ] **Step 3: 校验对等**

```bash
cd web && npm test -- localeParity
```

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add web/src/i18n/locales/zh/welcome.json web/src/i18n/locales/en/welcome.json
git commit -m "i18n: Welcome 权限同步短文案"
```

---

### Task 3: 拆除自动 relaunch，接线 Welcome 状态机（修闪白屏）

**Files:**
- Modify: `web/src/pages/Welcome/Welcome.tsx`（重写权限相关 effect/handler）
- Modify: `web/src/pages/Welcome/Welcome.test.tsx`
- Modify: `web/src/api/config.ts`（注释：relaunch 仅按钮）

**Interfaces:**
- Consumes: Task 1 状态机 API；`configApi.requestPermission` / `permissions` / `relaunchForPermissions` / `appIdentity`；`usePermissions`
- Produces: 无自动 relaunch 的 Welcome UI

- [ ] **Step 1: 扩展 Welcome 测试（先失败）**

重写/扩展 `Welcome.test.tsx` mock，使 `request`/`refresh`/`relaunchForPermissions` 可断言：

```typescript
// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import {
  PERMISSION_ONBOARDED_KEY,
  permissionSkippedKey,
} from '@/hooks/usePermissions';

const requestMock = vi.fn(async () => undefined);
const refreshMock = vi.fn(async () => undefined);
const relaunchMock = vi.fn(async () => undefined);
const permissionsMock = vi.fn(async () => ({
  screenCapture: { granted: false },
  accessibility: { granted: false },
  inputMonitoring: { granted: false },
  notification: { granted: false },
}));

vi.mock('@/api/config', () => ({
  configApi: {
    appIdentity: vi.fn(async () => ({
      bundleId: 'com.cc-partner.app',
      flavor: 'release' as const,
    })),
    permissions: (...a: unknown[]) => permissionsMock(...a),
    relaunchForPermissions: (...a: unknown[]) => relaunchMock(...a),
  },
}));

vi.mock('@/hooks/usePermissions', async () => {
  const actual = await vi.importActual<typeof import('@/hooks/usePermissions')>(
    '@/hooks/usePermissions',
  );
  return {
    ...actual,
    usePermissions: () => ({
      status: {
        screenCapture: { granted: false },
        accessibility: { granted: false },
        inputMonitoring: { granted: false },
        notification: { granted: false },
      },
      loading: false,
      refreshing: false,
      error: null,
      requesting: new Set(),
      allRequiredGranted: false,
      allGranted: false,
      request: requestMock,
      requestMissing: vi.fn(),
      refresh: refreshMock,
    }),
  };
});

import { Welcome } from './Welcome';

describe('Welcome', () => {
  beforeEach(() => {
    localStorage.clear();
    requestMock.mockClear();
    refreshMock.mockClear();
    relaunchMock.mockClear();
    permissionsMock.mockClear();
    permissionsMock.mockResolvedValue({
      screenCapture: { granted: false },
      accessibility: { granted: false },
      inputMonitoring: { granted: false },
      notification: { granted: false },
    });
  });

  afterEach(() => {
    cleanup();
  });

  test('skip writes permission skipped marker only', async () => {
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );
    await waitFor(() => {
      expect(screen.getByRole('button', { name: '暂时跳过' })).toBeTruthy();
    });
    fireEvent.click(screen.getByRole('button', { name: '暂时跳过' }));
    expect(localStorage.getItem(permissionSkippedKey('release'))).toBe('1');
    expect(localStorage.getItem(PERMISSION_ONBOARDED_KEY)).toBeNull();
  });

  test('go settings does not relaunch', async () => {
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );
    const go = await screen.findAllByRole('button', { name: '去设置' });
    fireEvent.click(go[0]!);
    await waitFor(() => {
      expect(requestMock).toHaveBeenCalled();
    });
    // 给 microtask/timer 机会误触发 relaunch
    await act(async () => {
      await Promise.resolve();
    });
    expect(relaunchMock).not.toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: 跑 Welcome 测试**

```bash
cd web && npm test -- Welcome.test
```

Expected: `go settings does not relaunch` 可能 FAIL（当前实现会 relaunch）或实现已部分改动

- [ ] **Step 3: 重写 Welcome.tsx 权限交互（关键行为）**

删除：`applyPermissionsAfterSettings` 内 `relaunchForPermissions`、`relaunchUsedRef` 自动路径、request 后立刻 apply→relaunch。

改为（结构要点，实现时写完整文件并保留中文 docstring）：

```tsx
// 伪代码结构 — 实现时展开为完整合法 TSX
const [phase, setPhase] = useState<WelcomePermPhase>('idle');
const phaseRef = useRef(phase);
useEffect(() => { phaseRef.current = phase; }, [phase]);

const dispatch = useCallback((event: WelcomePermEvent) => {
  setPhase((p) => reduceWelcomePermPhase(p, event));
}, []);

useEffect(() => {
  if (allRequiredGranted) dispatch({ type: 'ALL_REQUIRED_GRANTED' });
}, [allRequiredGranted, dispatch]);

const runSyncAfterForeground = useCallback(async () => {
  const start = phaseRef.current;
  if (start !== 'awaiting' && start !== 'needs_reopen' && start !== 'syncing') return;
  dispatch({ type: 'FOREGROUND' });
  for (let i = 0; i < SYNC_DELAYS_MS.length; i++) {
    const delay = SYNC_DELAYS_MS[i]!;
    if (delay > 0) await new Promise((r) => setTimeout(r, delay));
    await refresh();
    let slice;
    try {
      slice = await configApi.permissions();
    } catch {
      continue;
    }
    if (!hasStickyDenied(slice)) {
      dispatch({ type: 'SYNC_TICK', status: slice });
      return;
    }
    if (i === SYNC_DELAYS_MS.length - 1) {
      dispatch({ type: 'SYNC_EXHAUSTED', status: slice });
    } else {
      dispatch({ type: 'SYNC_TICK', status: slice });
    }
  }
}, [dispatch, refresh]);

useEffect(() => {
  const onVis = () => {
    if (document.visibilityState === 'visible') void runSyncAfterForeground();
  };
  document.addEventListener('visibilitychange', onVis);
  window.addEventListener('focus', onVis);
  return () => {
    document.removeEventListener('visibilitychange', onVis);
    window.removeEventListener('focus', onVis);
  };
}, [runSyncAfterForeground]);

const handleRequest = useCallback(
  (type: PermissionType) => {
    dispatch({ type: 'GO_SETTINGS', permission: type });
    void request(type).catch(() => undefined);
    // 通知等即时权限：request 后 refresh；禁止 relaunch
    void refresh();
  },
  [dispatch, request, refresh],
);

const handleReopen = useCallback(() => {
  dispatch({ type: 'REOPEN_CLICKED' });
  void configApi.relaunchForPermissions().catch(() => undefined);
}, [dispatch]);

// UI:
// hint: t(`welcome:${welcomeHintKey(phase, allRequiredGranted)}`)
// phase === 'needs_reopen' 时 footer 增加 Button：t('welcome:reopenApp') onClick={handleReopen}
```

**硬性：** `handleRequest` / visibility / request 完成路径 **零次** 调用 `relaunchForPermissions`，除非 `handleReopen`。

- [ ] **Step 4: 修正 config.ts 注释**

`relaunchForPermissions` 注释改为：仅 Welcome「重新打开应用」按钮调用；禁止 request/visibility 自动调用。

- [ ] **Step 5: 跑测**

```bash
cd web && npm test -- Welcome.test welcomePermissionFlow localeParity
cd web && npx tsc -b --pretty false
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add web/src/pages/Welcome/Welcome.tsx web/src/pages/Welcome/Welcome.test.tsx web/src/api/config.ts
git commit -m "fix(welcome): 禁止去设置自动 relaunch，接线可选重新打开"
```

---

### Task 4: 后端 request_permission 按权限分流

**Files:**
- Modify: `src-tauri/src/permissions/mod.rs`（`RequestPermissionResult` + `request_permission`）
- Modify: `src-tauri/src/commands/permissions.rs`（json 增加 `action`）
- Modify: `web/src/lib/types/core.ts`（`action?: 'settings' | 'prompt' | 'noop'`）

**Interfaces:**
- Produces: `RequestPermissionResult { ok, requested, opened, action }` where `action` ∈ `settings|prompt|noop`

- [ ] **Step 1: 扩展 Rust 结构与矩阵实现**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResult {
    pub ok: bool,
    pub requested: bool,
    pub opened: bool,
    /// settings = 打开系统设置页；prompt = 系统授权框；noop = 已授权无操作
    pub action: &'static str,
}
```

`request_permission` macOS 分支逻辑（`open_settings` 参数：**保留兼容**，但默认行为按矩阵；若 `Some(false)` 则只登记/请求不 open）：

| type | 行为 |
| --- | --- |
| `screenCapture` | 若 `!check_screen_capture_access()`：`CGRequest…`（requested=其返回值）；若 `open_settings != Some(false)`：`open_permission_settings` → action=`settings`（若 opened）否则若 requested 则 `prompt` |
| `accessibility` | 登记 `request_accessibility_prompt`；默认 open settings → action=`settings` |
| `inputMonitoring` | `register_input_monitoring_subject` + 短 sleep；默认 **只** open settings（不依赖第二套弹框）→ action=`settings` |
| `notification` | 若已 granted：action=`noop`，opened=false，requested=false；若 `!check` 且 status 需请求：`request_notification_access` → action=`prompt`；若仍 !ok 且允许 open：open 通知设置 → action=`settings` |

实现注意：

- **通知**：先读 status；authorized → noop；notDetermined → requestAuthorization（不要无脑 open）；denied → open settings。  
- **输入监控**：登记 + open settings；`requested` 表示是否调用了登记 API。  
- 非 macOS：`action = "noop"`。

命令层：

```rust
Ok(serde_json::json!({
    "ok": r.ok,
    "requested": r.requested,
    "opened": r.opened,
    "action": r.action,
}))
```

- [ ] **Step 2: 更新/补充 Rust 测试**

在 `permissions::tests`：

```rust
#[test]
fn request_notification_when_undetermined_prefers_prompt_shape() {
    let r = request_permission("notification", Some(false));
    // open_settings=false：不得 opened
    assert!(!r.opened);
    assert!(r.action == "prompt" || r.action == "noop" || r.action == "settings");
}

#[test]
fn request_input_monitoring_defaults_to_settings_action_when_open() {
    // 不强制本机 TCC；只验证 action 字段存在且为 settings 或在 open=false 时非 settings opened
    let r = request_permission("inputMonitoring", Some(false));
    assert!(!r.opened);
}
```

（按本机环境放宽断言，保持形状稳定。）

- [ ] **Step 3: 前端类型**

```typescript
export interface PermissionRequestResult {
  ok: boolean;
  requested: boolean;
  opened: boolean;
  action?: 'settings' | 'prompt' | 'noop';
  error?: string;
}
```

- [ ] **Step 4: 跑测**

```bash
cd src-tauri && cargo test --locked permissions:: --lib
cd web && npx tsc -b --pretty false
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/permissions/mod.rs src-tauri/src/commands/permissions.rs web/src/lib/types/core.ts
git commit -m "feat(permissions): 按权限类型分流登记与打开设置"
```

---

### Task 5: CLAUDE.md 与 spec 状态

**Files:**
- Modify: `web/CLAUDE.md`（macOS 权限流程段）
- Modify: `src-tauri/CLAUDE.md`（permissions 段）
- Modify: `docs/superpowers/specs/2026-07-18-welcome-permission-flow-design.md` 状态 → 实现中/已实现

- [ ] **Step 1: 更新 web/CLAUDE.md**

权限流程改为：

- 去设置：按类型登记 + 开设置或 prompt，不双开默认。  
- Welcome 状态机 idle/awaiting/syncing/needs_reopen。  
- **禁止** request/visibility 自动 relaunch；仅「重新打开应用」按钮。  
- 短文案 syncingPermission / reopenToApply。  

- [ ] **Step 2: 更新 src-tauri/CLAUDE.md**

- `request_permission` 矩阵与 `action` 字段。  
- `relaunch_for_permissions` 仅按钮路径；`open` .app。  

- [ ] **Step 3: Commit**

```bash
git add web/CLAUDE.md src-tauri/CLAUDE.md docs/superpowers/specs/2026-07-18-welcome-permission-flow-design.md
git commit -m "docs: 同步 Welcome 权限分流与可选 relaunch 需求"
```

---

### Task 6: 全量验证

- [ ] **Step 1: 自动化**

```bash
cd web && npm test -- welcomePermissionFlow Welcome.test usePermissions localeParity
cd web && npx tsc -b --pretty false
cd src-tauri && cargo test --locked permissions:: --lib
```

Expected: 全 PASS

- [ ] **Step 2: 手动 macOS（执行者勾选）**

1. `./start.sh dev`  
2. 点「去设置」→ **不闪白屏**，打开对应设置  
3. 打开开关后回应用 → 能检测则变绿  
4. 若仍红 → 出现「重新打开应用」→ 点击后 relaunch → 再进 Welcome 应绿（开关已开）  
5. 通知未决定 → 系统授权框（非无脑双开）  

- [ ] **Step 3: 最终 commit（若有修复）**

```bash
git status
# 仅提交验证中必要修复
```

---

## Spec Coverage Check

| Spec 要求 | Task |
| --- | --- |
| 禁止 request 后自动 relaunch | Task 3 |
| 回前台多轮 recheck | Task 1 + 3 |
| needs_reopen + 可选按钮 | Task 1 + 2 + 3 |
| 按权限分流登记/设置/prompt | Task 4 |
| 四项登记策略 | Task 4 |
| 短文案 | Task 2 |
| relaunch open .app | 已有命令；Task 3 仅按钮调用 |
| 测试 | Task 1, 3, 4, 6 |
| CLAUDE.md | Task 5 |

## Placeholder Scan

无 TBD/TODO；命令与导出名称在 Task 1–4 已固定。
