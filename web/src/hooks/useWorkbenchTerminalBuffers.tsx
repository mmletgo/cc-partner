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
 *   **authority 绑定 / 切换**（R9 M1 + R10 M1）：首次绑定与已绑定切换一律抬 epoch +
 *   needsReplay=true + 暂存新 authority live，并立即 sessions.replay；禁止 light rebind 且
 *   needsReplay=false（启动 list 无 projectId 仅本地，远端历史不会出现在 launch baseline）。
 *   **replay 失败恢复**（R10 M2 + R11 M1）：同 epoch 立即最多 3 次；耗尽后 capped backoff
 *   仅对 **可恢复** 错误（timeout/unavailable/network）持续重试，并允许后续 live 在 cooldown
 *   后重新触发，直到成功或 session 被移除。**永久错误**分类：not-found 终止并清理该 session
 *   的 replay 需求；validation/decode 停止自动重试并暴露 history_sync_failed 状态（禁止
 *   无限 3-burst + ~5s 静默循环）。
 *
 * Code Logic（这个模块做什么）:
 *   先注册 terminal-output / terminal-resync 监听，再对活跃 sessions 做 baseline replay；
 *   live 带 seq 的事件在 baseline 未 settle / needsReplay / replayInFlight 时写入有界 held；
 *   authority 变化走 beginAuthorityChangeReplay → requestSessionReplay（首次绑定亦强制）；
 *   resync/baseline 先 shouldAcceptTerminalCutover(authority)，再 applyTerminalBaselineCutover
 *   并 commitTerminalCutover(requestEpoch?, authorityId)；held 溢出先算 highWater 再清空
 *   held、beginHeldOverflowReplay 后 sessions.replay，in-flight 绑定 requestEpoch；
 *   replay catch 经 classifyTerminalReplayError 分流 recoverable / not_found / permanent。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { listen } from '@tauri-apps/api/event';
import { workbenchApi } from '@/api/workbench';
import type { WorkbenchTerminalOutputEvent } from '@/lib/types';
import {
  applyTerminalBaselineCutover,
  beginAuthorityChangeReplay,
  beginHeldOverflowReplay,
  classifyTerminalReplayError,
  commitTerminalCutover,
  createEmptySessionCutoverState,
  createWorkbenchTerminalBufferStore,
  isTerminalAuthorityChange,
  MAX_HELD_LIVE_TERMINAL_EVENTS,
  setTerminalCutoverReplayInFlight,
  shouldAcceptTerminalCutover,
  shouldCollectHeldLiveTerminalEvent,
  shouldTriggerTerminalReplayRecovery,
  stopTerminalCutoverReplay,
  terminalHistorySyncFailureFromClass,
  terminalReplayRecoveryDelayMs,
  TERMINAL_REPLAY_IMMEDIATE_ATTEMPTS,
  type HeldLiveTerminalEvent,
  type SessionCutoverState,
  type TerminalHistorySyncFailure,
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

/**
 * Business Logic（为什么需要这个接口）:
 *   同 epoch 立即重试耗尽后，仍需跨 live 事件与 timer 共享失败计数与 cooldown，
 *   避免瞬时故障下永久静默缺口或每个 live chunk 狂刷 replay（R10 M2）。
 *
 * Code Logic（这个接口做什么）:
 *   consecutiveFailures 累计失败次数；nextRetryAt 为可 live 触发的最早时刻；
 *   timerId 为可取消的恢复 setTimeout 句柄。
 *   仅 recoverable 错误写入本状态；永久错误走 historySyncFailureBySessionRef。
 */
interface SessionReplayRecoveryState {
  consecutiveFailures: number;
  nextRetryAt: number;
  timerId: ReturnType<typeof setTimeout> | null;
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
   * Business Logic（为什么需要这个 ref）:
   *   R10 M2：三次立即失败后仍要可恢复重试，需跨 timer/live 共享失败计数与 cooldown。
   *
   * Code Logic（这个 ref 做什么）:
   *   sessionId → SessionReplayRecoveryState；成功 cutover / removeBuffer / unmount 时清理。
   */
  const replayRecoveryBySessionRef = useRef<Map<string, SessionReplayRecoveryState>>(
    new Map(),
  );
  /**
   * Business Logic（为什么需要这个 ref）:
   *   R11 M1：永久 replay 错误必须停止自动重试并暴露可观察状态，禁止 silent infinite loop。
   *
   * Code Logic（这个 ref 做什么）:
   *   sessionId → TerminalHistorySyncFailure；成功 cutover / 新 authority / removeBuffer 清理。
   */
  const historySyncFailureBySessionRef = useRef<
    Map<string, TerminalHistorySyncFailure>
  >(new Map());

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

  /**
   * Business Logic（为什么需要这个函数）:
   *   session 被移除或 Provider 卸载时，必须取消挂起的恢复 timer，避免泄漏与对已删 session 狂刷。
   *
   * Code Logic（这个函数做什么）:
   *   clearTimeout 后从 map 删除该 session 的 recovery 条目。
   */
  const clearReplayRecovery = useCallback((sessionId: string): void => {
    const entry = replayRecoveryBySessionRef.current.get(sessionId);
    if (entry?.timerId != null) {
      globalThis.clearTimeout(entry.timerId);
    }
    replayRecoveryBySessionRef.current.delete(sessionId);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   成功 cutover / 新 authority 切换后必须清除旧的 history sync 失败标记。
   *
   * Code Logic（这个函数做什么）:
   *   从 historySyncFailureBySessionRef 删除该 session。
   */
  const clearHistorySyncFailure = useCallback((sessionId: string): void => {
    historySyncFailureBySessionRef.current.delete(sessionId);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   上层（诊断/后续 UI）需要读取 session 是否已停止 history 自动重试。
   *
   * Code Logic（这个函数做什么）:
   *   返回 map 中的 failure 或 null。
   */
  const getHistorySyncFailure = useCallback(
    (sessionId: string): TerminalHistorySyncFailure | null => {
      return historySyncFailureBySessionRef.current.get(sessionId) ?? null;
    },
    [],
  );

  const resetBuffer = useCallback((sessionId: string) => {
    store.reset(sessionId);
  }, [store]);

  const removeBuffer = useCallback((sessionId: string) => {
    heldLiveBySessionRef.current.delete(sessionId);
    cutoverBySessionRef.current.delete(sessionId);
    const entry = replayRecoveryBySessionRef.current.get(sessionId);
    if (entry?.timerId != null) {
      globalThis.clearTimeout(entry.timerId);
    }
    replayRecoveryBySessionRef.current.delete(sessionId);
    historySyncFailureBySessionRef.current.delete(sessionId);
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
     *   commitTerminalCutover(state, lastSeq, requestEpoch, authorityId) 回写 map；
     *   成功后清除该 session 的 replay recovery 与 history sync 失败状态。
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
      // 成功 baseline 后关闭可恢复重试闸门与永久失败标记。
      clearReplayRecovery(sessionId);
      clearHistorySyncFailure(sessionId);
      return true;
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   三次立即失败后需按 capped backoff 继续请求，直到成功或 session 移除（R10 M2）。
     *
     * Code Logic（这个函数做什么）:
     *   取消旧 timer → 按 consecutiveFailures 计算 delay → setTimeout 到期后若仍
     *   needsReplay/同 epoch/非 inFlight 则 requestSessionReplay(attempt=1)。
     */
    const scheduleRecoverableReplay = (
      sessionId: string,
      requestEpoch: number,
      consecutiveFailures: number,
    ): void => {
      const prev = replayRecoveryBySessionRef.current.get(sessionId);
      if (prev?.timerId != null) {
        globalThis.clearTimeout(prev.timerId);
      }
      const delayMs = terminalReplayRecoveryDelayMs(consecutiveFailures);
      const nextRetryAt = Date.now() + delayMs;
      const timerId = globalThis.setTimeout(() => {
        if (cancelled) return;
        const entry = replayRecoveryBySessionRef.current.get(sessionId);
        if (entry) {
          replayRecoveryBySessionRef.current.set(sessionId, {
            ...entry,
            timerId: null,
          });
        }
        const latest = ensureCutoverState(sessionId);
        if (
          latest.needsReplay &&
          latest.cutoverEpoch === requestEpoch &&
          !latest.replayInFlight
        ) {
          requestSessionReplay(sessionId, requestEpoch, 1);
        }
      }, delayMs);
      replayRecoveryBySessionRef.current.set(sessionId, {
        consecutiveFailures,
        nextRetryAt,
        timerId,
      });
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   立即重试耗尽后的恢复窗口内，后续 live 应能在 cooldown 后重新触发 replay，
     *   而不是干等 timer 或永久 stalled（R10 M2）。
     *
     * Code Logic（这个函数做什么）:
     *   仅当 recovery 条目存在、needsReplay、非 inFlight 且 cooldown 到期时：
     *   取消 pending timer 并 requestSessionReplay(attempt=1)。
     */
    const maybeLiveTriggerReplayRecovery = (sessionId: string): void => {
      const latest = ensureCutoverState(sessionId);
      if (!latest.needsReplay || latest.replayInFlight) {
        return;
      }
      const recovery = replayRecoveryBySessionRef.current.get(sessionId);
      if (!recovery) {
        return;
      }
      if (!shouldTriggerTerminalReplayRecovery(recovery.nextRetryAt, Date.now())) {
        return;
      }
      if (recovery.timerId != null) {
        globalThis.clearTimeout(recovery.timerId);
      }
      // 提前触发后刷新 cooldown，防止同波 live 连刷。
      const delayMs = terminalReplayRecoveryDelayMs(recovery.consecutiveFailures);
      replayRecoveryBySessionRef.current.set(sessionId, {
        consecutiveFailures: recovery.consecutiveFailures,
        nextRetryAt: Date.now() + delayMs,
        timerId: null,
      });
      requestSessionReplay(sessionId, latest.cutoverEpoch, 1);
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   held 溢出或显式 re-baseline 时需拉 sessions.replay；同 session 不得并发狂刷，
     *   但 overflow 抬 epoch 后旧 in-flight 结果必须失效并在 settle 后补拉；
     *   同 epoch **可恢复** 瞬时失败也不能立刻放弃，需有界立即重试 + 可恢复 backoff；
     *   **永久** not-found / validation/decode 必须停止自动重试（R11 M1）。
     *
     * Code Logic（这个函数做什么）:
     *   已有 replayInFlight 则仅依赖调用方已抬高的 epoch；否则标记 inFlight，
     *   await replay → applyCutover(requestEpoch)；catch 经 classifyTerminalReplayError：
     *   - recoverable：同 epoch 立即最多 TERMINAL_REPLAY_IMMEDIATE_ATTEMPTS 次，
     *     间隔 terminalReplayRecoveryDelayMs；耗尽后 scheduleRecoverableReplay；
     *   - not_found / permanent：clearReplayRecovery + stopTerminalCutoverReplay +
     *     写入 historySyncFailureBySessionRef，不再 schedule；
     *   finally 清 inFlight；若 needsReplay 且 epoch 已抬高则用新 epoch 再请求。
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
        let errorClass: ReturnType<typeof classifyTerminalReplayError> | null = null;
        try {
          const replay = await workbenchApi.sessions.replay(sessionId);
          if (cancelled) return;
          const authorityId =
            typeof replay.ownerInstanceId === 'string' &&
            replay.ownerInstanceId.length > 0
              ? replay.ownerInstanceId
              : null;
          applySucceeded = applyCutover(
            replay.sessionId,
            replay.buffer,
            replay.lastSeq,
            requestEpoch,
            authorityId,
          );
        } catch (reason) {
          applySucceeded = false;
          errorClass = classifyTerminalReplayError(reason);
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
            // 新 epoch 重置失败计数与永久失败标记。
            clearReplayRecovery(sessionId);
            clearHistorySyncFailure(sessionId);
            requestSessionReplay(sessionId, cleared.cutoverEpoch);
            return;
          }

          if (applySucceeded || !cleared.needsReplay) {
            clearReplayRecovery(sessionId);
            return;
          }

          if (
            cleared.cutoverEpoch !== requestEpoch ||
            cleared.replayInFlight
          ) {
            return;
          }

          // R11 M1：永久错误立即终止自动重试，禁止 3-burst + 5s 静默循环。
          if (errorClass === 'not_found' || errorClass === 'permanent') {
            clearReplayRecovery(sessionId);
            historySyncFailureBySessionRef.current.set(
              sessionId,
              terminalHistorySyncFailureFromClass(errorClass),
            );
            cutoverBySessionRef.current.set(
              sessionId,
              stopTerminalCutoverReplay(cleared),
            );
            // 受控诊断：仅稳定 kind，禁止打印 buffer/body/path/token。
            console.warn(
              '[workbench-terminal] history sync stopped',
              errorClass === 'not_found' ? 'not_found' : 'history_sync_failed',
            );
            return;
          }

          const prev = replayRecoveryBySessionRef.current.get(sessionId);
          const consecutiveFailures = (prev?.consecutiveFailures ?? 0) + 1;

          // 同 epoch 有界立即重试（最多 3 次，含首次）。
          // 立即窗口内也写入 cooldown，防止 held live 在 attempt 间隙狂刷。
          if (attempt < TERMINAL_REPLAY_IMMEDIATE_ATTEMPTS) {
            const delayMs = terminalReplayRecoveryDelayMs(attempt);
            if (prev?.timerId != null) {
              globalThis.clearTimeout(prev.timerId);
            }
            replayRecoveryBySessionRef.current.set(sessionId, {
              consecutiveFailures,
              nextRetryAt: Date.now() + delayMs,
              timerId: null,
            });
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
            return;
          }

          // 立即耗尽：进入 capped backoff 可恢复重试，直到成功或 session 移除。
          scheduleRecoverableReplay(sessionId, requestEpoch, consecutiveFailures);
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
      clearReplayRecovery(sessionId);
      clearHistorySyncFailure(sessionId);
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
            // 首次绑定 / 已绑定切换：一律强制 re-baseline（禁止 light rebind needsReplay=false）。
            const authoritySwitch = beginAuthorityChangeReplay(cutover, authorityId);
            if (authoritySwitch) {
              heldLiveBySessionRef.current.delete(sessionId);
              cutoverBySessionRef.current.set(sessionId, authoritySwitch.state);
              clearReplayRecovery(sessionId);
              clearHistorySyncFailure(sessionId);
              requestSessionReplay(sessionId, authoritySwitch.requestEpoch);
            } else {
              // 同 authority 下：若处于恢复窗口，live 可 cooldown 触发再请求。
              maybeLiveTriggerReplayRecovery(sessionId);
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
          const cutover = ensureCutoverState(sessionId);
          // resync 自身携带 baseline：首次绑定/已绑定切换时抬 epoch 作废 in-flight，
          // 但 needsReplay 由本次 applyCutover 的 requestEpoch 清掉（不是 live 强制 replay）。
          const authoritySwitch = beginAuthorityChangeReplay(cutover, authorityId);
          if (authoritySwitch) {
            heldLiveBySessionRef.current.delete(sessionId);
            clearReplayRecovery(sessionId);
            clearHistorySyncFailure(sessionId);
            cutoverBySessionRef.current.set(sessionId, {
              ...authoritySwitch.state,
              // resync 即 baseline：不需要再拉 sessions.replay
              needsReplay: false,
            });
            applyCutover(
              sessionId,
              payload.buffer ?? '',
              lastSeq,
              authoritySwitch.requestEpoch,
              authorityId,
            );
            return;
          }
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
      // 注意：sessions.list() 无 projectId 时仅枚举本机；远端 session 由后续 live/resync 强制 replay。
      try {
        const sessions = await workbenchApi.sessions.list();
        if (cancelled) return;
        await Promise.all(
          sessions.map(async (session) => {
            try {
              const replay = await workbenchApi.sessions.replay(session.id);
              if (cancelled) return;
              const authorityId =
                typeof replay.ownerInstanceId === 'string' &&
                replay.ownerInstanceId.length > 0
                  ? replay.ownerInstanceId
                  : null;
              applyCutover(
                replay.sessionId,
                replay.buffer,
                replay.lastSeq,
                undefined,
                authorityId,
              );
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
      for (const [sessionId, entry] of replayRecoveryBySessionRef.current) {
        if (entry.timerId != null) {
          globalThis.clearTimeout(entry.timerId);
        }
        replayRecoveryBySessionRef.current.delete(sessionId);
      }
      historySyncFailureBySessionRef.current.clear();
    };
  }, [clearHistorySyncFailure, clearReplayRecovery, ensureCutoverState, store]);

  const value = useMemo<WorkbenchTerminalBuffersContextValue>(
    () => ({
      store,
      resetBuffer,
      removeBuffer,
      getHistorySyncFailure,
    }),
    [getHistorySyncFailure, removeBuffer, resetBuffer, store],
  );

  return (
    <WorkbenchTerminalBuffersContext.Provider value={value}>
      {children}
    </WorkbenchTerminalBuffersContext.Provider>
  );
}
