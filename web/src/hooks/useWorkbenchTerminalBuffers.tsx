/**
 * Workbench 终端输出缓存 Provider。
 *
 * Business Logic（为什么需要这个模块）:
 *   用户离开 Workbench 路由后，后端 PTY/tmux 会继续输出；如果页面内监听被卸载，切回时 xterm
 *   会丢失 TUI 屏幕态并出现错位。缓存 Provider 必须跟随 AppShell 常驻。
 *
 * Code Logic（这个模块做什么）:
 *   监听 `workbench:terminal-output` 事件，写入 session 级外部 store，并提供重置/删除方法。
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { WorkbenchTerminalOutputEvent } from '@/lib/types';
import {
  createWorkbenchTerminalBufferStore,
  type WorkbenchTerminalBufferStore,
} from './workbenchTerminalBuffer';
import {
  WorkbenchTerminalBuffersContext,
  type WorkbenchTerminalBuffersContextValue,
} from './workbenchTerminalBuffersContext';

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

export interface WorkbenchTerminalBuffersProviderProps {
  children: ReactNode;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   普通浏览器调试环境没有 Tauri event internals，Provider 不应注册不可用的桌面事件。
 *
 * Code Logic（这个函数做什么）:
 *   检测 window.__TAURI_INTERNALS__.transformCallback 是否存在且为函数。
 */
function canListenToTauriEvents(): boolean {
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * WorkbenchTerminalBuffersProvider（工作台终端输出缓存）
 *
 * Business Logic（为什么需要这个组件）:
 *   终端输出缓存需要跨 Workbench 路由卸载保留，确保切出再切回时可 replay 已收到的 PTY/tmux 输出。
 *
 * Code Logic（这个组件做什么）:
 *   创建稳定的终端 buffer store，常驻监听后端 terminal-output 事件，并暴露 reset/remove 操作给 Workbench 页面。
 */
export function WorkbenchTerminalBuffersProvider({
  children,
}: WorkbenchTerminalBuffersProviderProps) {
  const [store] = useState<WorkbenchTerminalBufferStore>(() =>
    createWorkbenchTerminalBufferStore(),
  );

  const resetBuffer = useCallback((sessionId: string) => {
    store.reset(sessionId);
  }, [store]);

  const removeBuffer = useCallback((sessionId: string) => {
    store.remove(sessionId);
  }, [store]);

  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    const outputUnlisten = listen<WorkbenchTerminalOutputEvent>(
      'workbench:terminal-output',
      (event) => {
        const payload = event.payload;
        store.append(payload.sessionId, payload.chunk);
      },
    );
    return () => {
      void outputUnlisten.then((fn) => fn());
    };
  }, [store]);

  const value = useMemo<WorkbenchTerminalBuffersContextValue>(
    () => ({
      store,
      resetBuffer,
      removeBuffer,
    }),
    [removeBuffer, resetBuffer, store],
  );

  return (
    <WorkbenchTerminalBuffersContext.Provider value={value}>
      {children}
    </WorkbenchTerminalBuffersContext.Provider>
  );
}
