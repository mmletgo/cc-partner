/**
 * Workbench 终端输出缓存 Provider。
 *
 * Business Logic（为什么需要这个模块）:
 *   用户离开 Workbench 路由后，后端 PTY/tmux 会继续输出；如果页面内监听被卸载，切回时 xterm
 *   会丢失 TUI 屏幕态并出现错位。缓存 Provider 必须跟随 AppShell 常驻。
 *   桌面 GUI 在 React 挂载前已从 owner stream 转发 terminal-output，因此 Provider 必须
 *   listener-first + baseline replay，并用 lastSeq 丢弃 resync 后的重复 live 事件。
 *   乱序 baseline 与 held 溢出不得静默丢 chunk：per-session cutover epoch + committed lastSeq
 *   拒绝更旧 cutover；held 超限触发 re-baseline 而非 drop-oldest。
 *
 * Code Logic（这个模块做什么）:
 *   先注册 terminal-output / terminal-resync 监听，再对活跃 sessions 做 baseline replay；
 *   live 带 seq 的事件写入有界 held map；resync/baseline 先 shouldAcceptTerminalCutover，
 *   再 applyTerminalBaselineCutover 并 commitTerminalCutover；held 溢出清空 held、
 *   beginHeldOverflowReplay 后 sessions.replay，in-flight 绑定 requestEpoch。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { listen } from '@tauri-apps/api/event';
import { workbenchApi } from '@/api/workbench';
import type { WorkbenchTerminalOutputEvent } from '@/lib/types';
import {
  applyTerminalBaselineCutover,
  beginHeldOverflowReplay,
  commitTerminalCutover,
  createEmptySessionCutoverState,
  createWorkbenchTerminalBufferStore,
  MAX_HELD_LIVE_TERMINAL_EVENTS,
  setTerminalCutoverReplayInFlight,
  shouldAcceptTerminalCutover,
  type HeldLiveTerminalEvent,
  type SessionCutoverState,
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
  /**
   * Business Logic（为什么需要这个 ref）:
   *   乱序 baseline 与 held 溢出 re-baseline 需要 per-session 单调 epoch 与 committed lastSeq。
   *
   * Code Logic（这个 ref 做什么）:
   *   sessionId → SessionCutoverState；removeBuffer 时删除条目。
   */
  const cutoverBySessionRef = useRef<Map<string, SessionCutoverState>>(new Map());

  /**
   * Business Logic（为什么需要这个函数）:
   *   cutover helper 需要可写的 session 状态，避免调用方重复判空。
   *
   * Code Logic（这个函数做什么）:
   *   若 map 无该 session 则 createEmptySessionCutoverState 并登记后返回。
   */
  const ensureCutoverState = useCallback((sessionId: string): SessionCutoverState => {
    let state = cutoverBySessionRef.current.get(sessionId);
    if (!state) {
      state = createEmptySessionCutoverState();
      cutoverBySessionRef.current.set(sessionId, state);
    }
    return state;
  }, []);

  const resetBuffer = useCallback((sessionId: string) => {
    store.reset(sessionId);
  }, [store]);

  const removeBuffer = useCallback((sessionId: string) => {
    heldLiveBySessionRef.current.delete(sessionId);
    cutoverBySessionRef.current.delete(sessionId);
    store.remove(sessionId);
  }, [store]);

  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    let cancelled = false;
    let unlistenOutput: (() => void) | undefined;
    let unlistenResync: (() => void) | undefined;

    /**
     * Business Logic（为什么需要这个函数）:
     *   cutover 路径（resync / baseline replay）不能只 reset，且不能接受更旧 lastSeq/epoch。
     *
     * Code Logic（这个函数做什么）:
     *   shouldAccept → held + applyTerminalBaselineCutover → commitTerminalCutover 回写 map。
     */
    const applyCutover = (
      sessionId: string,
      buffer: string,
      lastSeq: number,
      requestEpoch?: number,
    ): boolean => {
      const state = ensureCutoverState(sessionId);
      if (!shouldAcceptTerminalCutover(state, lastSeq, requestEpoch)) {
        return false;
      }
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
      cutoverBySessionRef.current.set(
        sessionId,
        commitTerminalCutover(state, lastSeq),
      );
      return true;
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   held 溢出或显式 re-baseline 时需拉 sessions.replay；同 session 不得并发狂刷，
     *   但 overflow 抬 epoch 后旧 in-flight 结果必须失效并在 settle 后补拉。
     *
     * Code Logic（这个函数做什么）:
     *   已有 replayInFlight 则仅依赖调用方已抬高的 epoch；否则标记 inFlight 并
     *   await replay → applyCutover(requestEpoch)；finally 清 inFlight，若 needsReplay
     *   且 epoch 已抬高则用当前 epoch 再请求一次。
     */
    const requestSessionReplay = (sessionId: string, requestEpoch: number): void => {
      const state = ensureCutoverState(sessionId);
      if (state.replayInFlight) {
        return;
      }
      cutoverBySessionRef.current.set(
        sessionId,
        setTerminalCutoverReplayInFlight(state, true),
      );

      void (async () => {
        try {
          const replay = await workbenchApi.sessions.replay(sessionId);
          if (cancelled) return;
          applyCutover(
            replay.sessionId,
            replay.buffer,
            replay.lastSeq,
            requestEpoch,
          );
        } catch {
          // 单次 replay 失败不狂刷；needsReplay 可保留至后续 overflow / terminal-resync。
        } finally {
          if (cancelled) return;
          const current = ensureCutoverState(sessionId);
          const cleared = setTerminalCutoverReplayInFlight(current, false);
          cutoverBySessionRef.current.set(sessionId, cleared);
          // in-flight 期间 overflow 抬高了 epoch：旧结果已失效，需用新 epoch 再拉一次。
          if (
            cleared.needsReplay &&
            cleared.cutoverEpoch > requestEpoch &&
            !cleared.replayInFlight
          ) {
            requestSessionReplay(sessionId, cleared.cutoverEpoch);
          }
        }
      })();
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   held 超过 256 条时静默 drop-oldest 会留下 relay 永不补发的缺口。
     *
     * Code Logic（这个函数做什么）:
     *   清空 held → beginHeldOverflowReplay → 必要时 requestSessionReplay。
     */
    const handleHeldOverflow = (sessionId: string): void => {
      heldLiveBySessionRef.current.set(sessionId, []);
      const state = ensureCutoverState(sessionId);
      const { state: next, requestEpoch } = beginHeldOverflowReplay(state);
      cutoverBySessionRef.current.set(sessionId, next);
      requestSessionReplay(sessionId, requestEpoch);
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   必须先挂 listener 再 baseline，避免 subscribe 与 replay 之间的 live 输出永久丢失。
     *
     * Code Logic（这个函数做什么）:
     *   await 两个 listen；output 暂存带 seq live 后 append，超限走 overflow re-baseline；
     *   resync/baseline 走 cutover；随后 list 全部 session 并逐个 replay baseline。
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
                // 禁止 splice drop-oldest；清空 held 并 re-baseline。
                handleHeldOverflow(sessionId);
              } else {
                heldLiveBySessionRef.current.set(sessionId, held);
              }
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
  }, [ensureCutoverState, store]);

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
