/**
 * Workbench 终端输出缓存 Provider。
 *
 * Business Logic（为什么需要这个模块）:
 *   用户离开 Workbench 路由后，后端 PTY/tmux 会继续输出；如果页面内监听被卸载，切回时 xterm
 *   会丢失 TUI 屏幕态并出现错位。缓存 Provider 必须跟随 AppShell 常驻。
 *   桌面 GUI 在 React 挂载前已从 owner stream 转发 terminal-output，因此 Provider 必须
 *   listener-first + baseline replay，并用 lastSeq 丢弃 resync 后的重复 live 事件。
 *   乱序 baseline 与 held 溢出不得静默丢 chunk：per-session cutover epoch + committed lastSeq
 *   拒绝更旧 cutover；held 仅在异步 baseline/replay 窗口收集，steady-state 不入 held；
 *   超限触发 re-baseline 而非 drop-oldest；ownerInstanceId 分代避免 owner 重启后 seq 冻结。
 *
 * Code Logic（这个模块做什么）:
 *   先注册 terminal-output / terminal-resync 监听，再对活跃 sessions 做 baseline replay；
 *   live 带 seq 的事件在 baseline 未 settle / needsReplay / replayInFlight 时写入有界 held；
 *   resync/baseline 先 shouldAcceptTerminalCutover(authority)，再 applyTerminalBaselineCutover
 *   并 commitTerminalCutover(requestEpoch?, authorityId)；held 溢出先算 highWater 再清空
 *   held、beginHeldOverflowReplay 后 sessions.replay，in-flight 绑定 requestEpoch。
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
  isTerminalAuthorityChange,
  MAX_HELD_LIVE_TERMINAL_EVENTS,
  setTerminalCutoverReplayInFlight,
  shouldAcceptTerminalCutover,
  shouldCollectHeldLiveTerminalEvent,
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
     *   cutover 路径（resync / baseline replay）不能只 reset，且不能接受更旧 lastSeq/epoch；
     *   owner 切换时必须接受新 authority 的较低 lastSeq；commit 时必须把 requestEpoch 与
     *   authorityId 传给 helper，否则 stale 无 epoch baseline 会误清 needsReplay 或冻结终端。
     *
     * Code Logic（这个函数做什么）:
     *   shouldAccept(authority) → held + applyTerminalBaselineCutover(authorityChanged) →
     *   commitTerminalCutover(state, lastSeq, requestEpoch, authorityId) 回写 map。
     */
    const applyCutover = (
      sessionId: string,
      buffer: string,
      lastSeq: number,
      requestEpoch?: number,
      authorityId?: string | null,
    ): boolean => {
      const state = ensureCutoverState(sessionId);
      if (!shouldAcceptTerminalCutover(state, lastSeq, requestEpoch, authorityId)) {
        return false;
      }
      const authorityChanged = isTerminalAuthorityChange(state, authorityId);
      const held = heldLiveBySessionRef.current.get(sessionId) ?? [];
      const pruned = applyTerminalBaselineCutover(
        store,
        sessionId,
        buffer,
        lastSeq,
        held,
        authorityId,
        authorityChanged,
      );
      if (pruned.length === 0) {
        heldLiveBySessionRef.current.delete(sessionId);
      } else {
        heldLiveBySessionRef.current.set(sessionId, pruned);
      }
      cutoverBySessionRef.current.set(
        sessionId,
        commitTerminalCutover(state, lastSeq, requestEpoch, authorityId),
      );
      return true;
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   held 溢出或显式 re-baseline 时需拉 sessions.replay；同 session 不得并发狂刷，
     *   但 overflow 抬 epoch 后旧 in-flight 结果必须失效并在 settle 后补拉；
     *   同 epoch 瞬时失败也不能立刻放弃，需有界重试以免永久缺口。
     *
     * Code Logic（这个函数做什么）:
     *   已有 replayInFlight 则仅依赖调用方已抬高的 epoch；否则标记 inFlight，
     *   最多 3 次（含首次）await replay → applyCutover(requestEpoch)，失败间隔
     *   50/100/200ms；finally 清 inFlight，若 needsReplay 且 epoch 已抬高则用新 epoch
     *   再请求；同 epoch 最终失败则保留 needsReplay。
     */
    const requestSessionReplay = (
      sessionId: string,
      requestEpoch: number,
      attempt = 1,
    ): void => {
      const state = ensureCutoverState(sessionId);
      if (state.replayInFlight) {
        return;
      }
      cutoverBySessionRef.current.set(
        sessionId,
        setTerminalCutoverReplayInFlight(state, true),
      );

      void (async () => {
        let applySucceeded = false;
        try {
          const replay = await workbenchApi.sessions.replay(sessionId);
          if (cancelled) return;
          applySucceeded = applyCutover(
            replay.sessionId,
            replay.buffer,
            replay.lastSeq,
            requestEpoch,
          );
        } catch {
          applySucceeded = false;
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
            return;
          }

          // 同 epoch 失败：有界重试（最多 3 次，含首次），避免瞬时失败变永久缺口。
          if (
            !applySucceeded &&
            cleared.needsReplay &&
            cleared.cutoverEpoch === requestEpoch &&
            !cleared.replayInFlight &&
            attempt < 3
          ) {
            const delayMs = attempt === 1 ? 50 : attempt === 2 ? 100 : 200;
            globalThis.setTimeout(() => {
              if (cancelled) return;
              const latest = ensureCutoverState(sessionId);
              if (
                latest.needsReplay &&
                latest.cutoverEpoch === requestEpoch &&
                !latest.replayInFlight
              ) {
                requestSessionReplay(sessionId, requestEpoch, attempt + 1);
              }
            }, delayMs);
          }
        }
      })();
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   held 超过 256 条时静默 drop-oldest 会留下 relay 永不补发的缺口。
     *
     * Code Logic（这个函数做什么）:
     *   清空 held 前先取 held 中最大有限 seq 作 highWater → beginHeldOverflowReplay
     *   → requestSessionReplay。
     */
    const handleHeldOverflow = (sessionId: string): void => {
      const held = heldLiveBySessionRef.current.get(sessionId) ?? [];
      let highWater = 0;
      for (const event of held) {
        if (typeof event.seq === 'number' && Number.isFinite(event.seq)) {
          highWater = Math.max(highWater, event.seq);
        }
      }
      heldLiveBySessionRef.current.set(sessionId, []);
      const state = ensureCutoverState(sessionId);
      const { state: next, requestEpoch } = beginHeldOverflowReplay(
        state,
        highWater,
      );
      cutoverBySessionRef.current.set(sessionId, next);
      requestSessionReplay(sessionId, requestEpoch);
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   必须先挂 listener 再 baseline，避免 subscribe 与 replay 之间的 live 输出永久丢失。
     *
     * Code Logic（这个函数做什么）:
     *   await 两个 listen；output 仅在 baseline 窗口/needsReplay/replayInFlight 时 held，
     *   再 append（带 authority）；超限走 overflow re-baseline；resync/baseline 走 cutover；
     *   随后 list 全部 session 并逐个 replay baseline，完成后标记 baselineSettled。
     */
    const setup = async (): Promise<void> => {
      // baseline 未 settle 前 live 可能先于 replay 到达，必须 held；settle 后清空 held 路径。
      let baselineSettled = false;
      try {
        unlistenOutput = await listen<
          WorkbenchTerminalOutputEvent & { ownerInstanceId?: string }
        >('workbench:terminal-output', (event) => {
          const payload = event.payload;
          if (!payload?.sessionId) return;
          const sessionId = payload.sessionId;
          const chunk = payload.chunk ?? '';
          const seq = payload.seq;
          const authorityId =
            typeof payload.ownerInstanceId === 'string' &&
            payload.ownerInstanceId.length > 0
              ? payload.ownerInstanceId
              : null;
          if (typeof seq === 'number' && Number.isFinite(seq)) {
            const cutover = ensureCutoverState(sessionId);
            if (isTerminalAuthorityChange(cutover, authorityId)) {
              // live 先于 resync 到达新 owner 时：清空旧 held 与 lastSeq 基线，绑定新 authority。
              heldLiveBySessionRef.current.delete(sessionId);
              cutoverBySessionRef.current.set(sessionId, {
                ...cutover,
                authorityId: authorityId ?? cutover.authorityId,
                committedBaselineLastSeq: 0,
                overflowHighWaterSeq: 0,
                needsReplay: false,
              });
            }
            const collectState = ensureCutoverState(sessionId);
            if (shouldCollectHeldLiveTerminalEvent(collectState, baselineSettled)) {
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
          }
          store.append(sessionId, chunk, seq, authorityId);
        });
        unlistenResync = await listen<{
          sessionId?: string;
          buffer?: string;
          lastSeq?: number;
          ownerInstanceId?: string;
        }>('workbench:terminal-resync', (event) => {
          const payload = event.payload;
          const sessionId = payload.sessionId;
          if (!sessionId) return;
          const lastSeq =
            typeof payload.lastSeq === 'number' && Number.isFinite(payload.lastSeq)
              ? payload.lastSeq
              : 0;
          const authorityId =
            typeof payload.ownerInstanceId === 'string' &&
            payload.ownerInstanceId.length > 0
              ? payload.ownerInstanceId
              : null;
          applyCutover(sessionId, payload.buffer ?? '', lastSeq, undefined, authorityId);
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
      } finally {
        if (!cancelled) {
          baselineSettled = true;
        }
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
