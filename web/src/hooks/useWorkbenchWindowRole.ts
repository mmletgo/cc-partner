/**
 * 当前 OS 窗角色 hook（非第 8 个业务 controller）。
 *
 * Business Logic（为什么需要这个模块）:
 *   卫星窗与主窗共用同一套 React 树，layout / 监听器 / chrome 必须按 Tauri label 分流。
 *
 * Code Logic（这个模块做什么）:
 *   同步读取 `getCurrentWindow().label`（无 Tauri 回落 main），派生 role 与 auto slot。
 */

import { useMemo, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  layoutSlotKeyForWindowLabel,
  MAIN_WINDOW_LABEL,
  parseWorkbenchWindowRole,
  type WorkbenchWindowRole,
} from '@/lib/workbenchWindow';

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: unknown;
}

export interface WorkbenchWindowRoleValue {
  role: WorkbenchWindowRole;
  label: string;
  layoutSlotKey: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   首帧就必须知道是不是卫星窗，不能等 effect 后再卸掉全局通知监听器。
 *
 * Code Logic（这个函数做什么）:
 *   有 Tauri internals 时读当前窗 label，失败回落 `main`。
 */
export function readCurrentWindowLabel(): string {
  if (typeof window === 'undefined') return MAIN_WINDOW_LABEL;
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  if (!internals) return MAIN_WINDOW_LABEL;
  try {
    const label = getCurrentWindow().label?.trim();
    return label || MAIN_WINDOW_LABEL;
  } catch {
    return MAIN_WINDOW_LABEL;
  }
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   AppShell / restore / 全局监听器需要同一份窗口身份。
 *
 * Code Logic（这个 hook 做什么）:
 *   用同步初值固定本 webview 的 label，再派生 role 与 layoutSlotKey。
 */
export function useWorkbenchWindowRole(): WorkbenchWindowRoleValue {
  const [label] = useState(readCurrentWindowLabel);
  return useMemo(() => {
    const role = parseWorkbenchWindowRole(label);
    return {
      role,
      label,
      layoutSlotKey: role === 'overlay' ? null : layoutSlotKeyForWindowLabel(label),
    };
  }, [label]);
}
