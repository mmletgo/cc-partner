/**
 * Workbench 终端输出缓存 Provider。
 *
 * Business Logic（为什么需要这个模块）:
 *   用户离开 Workbench 路由后，后端 PTY/tmux 会继续输出；如果页面内监听被卸载，切回时 xterm
 *   会丢失 TUI 屏幕态并出现错位。缓存 Provider 必须跟随 AppShell 常驻。
 *   桌面 GUI 在 React 挂载前已从 owner stream 转发 terminal-output，因此 Provider 必须
 *   listener-first + baseline replay，并用 lastSeq 丢弃 resync 后的重复 live 事件。
 *
 * Code Logic（这个模块做什么）:
 *   先注册 terminal-output / terminal-resync 监听，再对活跃 sessions 做 baseline replay；
 *   live 带 seq 的事件写入有界 held map；resync/baseline 走 applyTerminalBaselineCutover
 *   （reset 后再 re-append seq > lastSeq 的 held live）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { listen } from '@tauri-apps/api/event';
import { workbenchApi } from '@/api/workbench';
import type { WorkbenchTerminalOutputEvent } from '@/lib/types';
import {
  applyTerminalBaselineCutover,
  createWorkbenchTerminalBufferStore,
  MAX_HELD_LIVE_TERMINAL_EVENTS,
  type HeldLiveTerminalEvent,
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
 *   以 options 形式创建稳定的 session 级 ring-buffer store（默认 rAF 帧批处理），
 *   listener-first 注册后 baseline replay，常驻监听 terminal-output/resync，暴露 reset/remove。
 */
export function WorkbenchTerminalBuffersProvider({
  children,
}: WorkbenchTerminalBuffersProviderProps) {
  const [store] = useState<WorkbenchTerminalBufferStore>(() =>
    createWorkbenchTerminalBufferStore(),
  );
  /**
   * Business Logic（为什么需要这个 ref）:
   *   baseline/resync 异步完成前 live 事件可能已带更新 seq 到达；必须跨 listener 闭包
   *   稳定暂存，供 cutover 后写回，否则 reset 会永久抹掉 relay 不会重发的 chunk。
   *
   * Code Logic（这个 ref 做什么）:
   *   sessionId → 最近有界 HeldLiveTerminalEvent[]；listener 只读写 ref.current。
   */
  const heldLiveBySessionRef = useRef<Map<string, HeldLiveTerminalEvent[]>>(
    new Map(),
  );

  const resetBuffer = useCallback((sessionId: string) => {
    store.reset(sessionId);
  }, [store]);

  const removeBuffer = useCallback((sessionId: string) => {
    heldLiveBySessionRef.current.delete(sessionId);
    store.remove(sessionId);
  }, [store]);

  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    let cancelled = false;
    let unlistenOutput: (() => void) | undefined;
    let unlistenResync: (() => void) | undefined;

    /**
     * Business Logic（为什么需要这个函数）:
     *   cutover 路径（resync / baseline replay）不能只 reset，否则会抹掉 held 中更新的 live。
     *
     * Code Logic（这个函数做什么）:
     *   读取 session held → applyTerminalBaselineCutover → 用 pruned 列表回写 map。
     */
    const applyCutover = (
      sessionId: string,
      buffer: string,
      lastSeq: number,
    ): void => {
      const held = heldLiveBySessionRef.current.get(sessionId) ?? [];
      const pruned = applyTerminalBaselineCutover(
        store,
        sessionId,
        buffer,
        lastSeq,
        held,
      );
      if (pruned.length === 0) {
        heldLiveBySessionRef.current.delete(sessionId);
      } else {
        heldLiveBySessionRef.current.set(sessionId, pruned);
      }
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   必须先挂 listener 再 baseline，避免 subscribe 与 replay 之间的 live 输出永久丢失。
     *
     * Code Logic（这个函数做什么）:
     *   await 两个 listen；output 暂存带 seq live 后 append；resync/baseline 走 cutover；
     *   随后 list 全部 session 并逐个 replay baseline（单 session 失败跳过）。
     */
    const setup = async (): Promise<void> => {
      try {
        unlistenOutput = await listen<WorkbenchTerminalOutputEvent>(
          'workbench:terminal-output',
          (event) => {
            const payload = event.payload;
            if (!payload?.sessionId) return;
            const sessionId = payload.sessionId;
            const chunk = payload.chunk ?? '';
            const seq = payload.seq;
            if (typeof seq === 'number' && Number.isFinite(seq)) {
              const held =
                heldLiveBySessionRef.current.get(sessionId) ?? [];
              held.push({ chunk, seq });
              if (held.length > MAX_HELD_LIVE_TERMINAL_EVENTS) {
                held.splice(0, held.length - MAX_HELD_LIVE_TERMINAL_EVENTS);
              }
              heldLiveBySessionRef.current.set(sessionId, held);
            }
            store.append(sessionId, chunk, seq);
          },
        );
        unlistenResync = await listen<{
          sessionId?: string;
          buffer?: string;
          lastSeq?: number;
        }>('workbench:terminal-resync', (event) => {
          const payload = event.payload;
          const sessionId = payload.sessionId;
          if (!sessionId) return;
          const lastSeq =
            typeof payload.lastSeq === 'number' && Number.isFinite(payload.lastSeq)
              ? payload.lastSeq
              : 0;
          applyCutover(sessionId, payload.buffer ?? '', lastSeq);
        });
      } catch {
        // 非 Tauri 或 listen 失败：不 baseline
        return;
      }

      if (cancelled) {
        unlistenOutput?.();
        unlistenResync?.();
        return;
      }

      // baseline：补上 React 挂载前已从 owner ring 发出的输出，并建立 lastSeq cutover。
      try {
        const sessions = await workbenchApi.sessions.list();
        if (cancelled) return;
        await Promise.all(
          sessions.map(async (session) => {
            try {
              const replay = await workbenchApi.sessions.replay(session.id);
              if (cancelled) return;
              applyCutover(replay.sessionId, replay.buffer, replay.lastSeq);
            } catch {
              // 单 session replay 失败不阻断其它 session；后续 resync / live 仍可恢复。
            }
          }),
        );
      } catch {
        // list 失败：依赖后续 terminal-resync 与项目 loadSessions 路径闭合 race。
      }
    };

    void setup();
    return () => {
      cancelled = true;
      unlistenOutput?.();
      unlistenResync?.();
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
