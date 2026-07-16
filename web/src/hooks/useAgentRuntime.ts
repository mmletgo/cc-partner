/**
 * Agent Runtime snapshot→listen handshake hook。
 *
 * Business Logic（为什么需要这个模块）:
 *   Desktop 需要把 A1 owner 真值自动投影到 terminal selector；Gap/owner restart
 *   不得丢事件或用旧 version 覆盖，且不得把 Orchestrator 旧 Claude 字段当 Agent 状态。
 *
 * Code Logic（这个模块做什么）:
 *   先注册 Tauri listener 并缓冲 (owner,sequence)，再拉 snapshot baseline，
 *   丢弃 sequence<=asOfSequence，drain 后续事件后进入 live；提供 state/selector。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  BACKEND_RUNTIME_GAP_EVENT,
  WORKBENCH_AGENT_RUNTIME_EVENT,
  workbenchApi,
} from '@/api/workbench';
import {
  applyAgentRuntimeEvent,
  applyAgentRuntimeSnapshot,
  emptyAgentRuntimeState,
  latestAgentForTerminal as selectLatestAgentForTerminal,
  markAgentRuntimeFreshness,
} from '@/lib/agentRuntimeState';
import { agentRuntimeEventDecoder, agentRuntimeSnapshotDecoder } from '@/lib/schemas/agentRuntime';
import type {
  AgentRuntimeEvent,
  AgentRuntimeSnapshot,
  AgentRuntimeState,
  AgentSessionProjection,
} from '@/lib/types/agentRuntime';

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

type HandshakePhase = 'pending' | 'live' | 'error';

/**
 * Business Logic（为什么需要这个函数）:
 *   普通浏览器/Playwright 无 Tauri internals，listen 会失败。
 *
 * Code Logic（这个函数做什么）:
 *   检测 transformCallback 是否为函数。
 */
function canListenToTauriEvents(): boolean {
  if (typeof window === 'undefined') return false;
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live payload 可能是纯 agentSession 或 relay 注入了 owner/sequence 的扩展对象。
 *
 * Code Logic（这个函数做什么）:
 *   接受 {agentSession,...} 或直接包一层；decode fail-closed 返回 null。
 */
export function normalizeAgentRuntimeEvent(raw: unknown): AgentRuntimeEvent | null {
  if (!raw || typeof raw !== 'object') return null;
  try {
    // 已是 { agentSession }
    return agentRuntimeEventDecoder.decode(raw);
  } catch {
    // 可能是扁平 DTO（仅 session 字段）
    try {
      const asSession = agentRuntimeEventDecoder.decode({ agentSession: raw });
      return asSession;
    } catch {
      return null;
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   缓冲事件需按 sequence 顺序 drain，与 owner event bus 序一致。
 *
 * Code Logic（这个函数做什么）:
 *   sequence 升序；缺失 sequence 排后。
 */
function sortBufferedEvents(events: AgentRuntimeEvent[]): AgentRuntimeEvent[] {
  return [...events].sort((a, b) => {
    const sa = typeof a.sequence === 'number' ? a.sequence : Number.MAX_SAFE_INTEGER;
    const sb = typeof b.sequence === 'number' ? b.sequence : Number.MAX_SAFE_INTEGER;
    if (sa !== sb) return sa - sb;
    return 0;
  });
}

/**
 * useAgentRuntime 返回值。
 *
 * Business Logic（为什么需要这个类型）:
 *   页面/terminal 只读 selector，不直接持有 Map 可变引用。
 *
 * Code Logic（字段说明）:
 *   state 为聚合；latestAgentForTerminal 为便捷 selector；
 *   phase=error 表示 snapshot 失败（非永久 pending）；refresh 可重试握手。
 */
export interface UseAgentRuntimeResult {
  state: AgentRuntimeState;
  phase: HandshakePhase;
  error: Error | null;
  latestAgentForTerminal: (terminalSessionId: string) => AgentSessionProjection | null;
  /** 重新拉取 snapshot baseline（snapshot 失败或 Gap 后可调用）。 */
  refresh: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   Workbench 进入项目后需自动维护 Agent phase 投影，Gap 时重建，组件只消费 selector。
 *
 * Code Logic（这个 hook 做什么）:
 *   projectId 变化重置；listener-first handshake；可选 snapshot 注入供测试。
 *
 * @param projectId 当前项目；null/空则保持 empty 且不握手
 * @param options 可选 getSnapshot 覆盖（测试/HTTP）
 */
export function useAgentRuntime(
  projectId: string | null,
  options?: {
    getSnapshot?: (projectId: string | null) => Promise<AgentRuntimeSnapshot>;
    enabled?: boolean;
  },
): UseAgentRuntimeResult {
  const enabled = options?.enabled !== false;
  const injectedGetSnapshot = options?.getSnapshot;
  const getSnapshot = useCallback(
    (pid: string | null) =>
      injectedGetSnapshot
        ? injectedGetSnapshot(pid)
        : workbenchApi.agentRuntime.getSnapshot(pid),
    [injectedGetSnapshot],
  );

  const [state, setState] = useState<AgentRuntimeState>(() => emptyAgentRuntimeState());
  const [phase, setPhase] = useState<HandshakePhase>('pending');
  const [error, setError] = useState<Error | null>(null);
  // project/enabled 关闭时在 render 中复位投影，避免 setState-in-effect
  const runtimeActiveKey = enabled && projectId ? projectId : null;
  const [boundRuntimeKey, setBoundRuntimeKey] = useState<string | null>(runtimeActiveKey);
  if (boundRuntimeKey !== runtimeActiveKey) {
    setBoundRuntimeKey(runtimeActiveKey);
    if (runtimeActiveKey === null) {
      setPhase('pending');
      setState(emptyAgentRuntimeState());
      setError(null);
    }
  }

  const phaseRef = useRef<HandshakePhase>('pending');
  const bufferRef = useRef<AgentRuntimeEvent[]>([]);
  const cursorRef = useRef<{ ownerInstanceId: string; sequence: number } | null>(null);
  const handshakeGenerationRef = useRef(0);
  const stateRef = useRef(state);
  const projectIdRef = useRef(projectId);
  const getSnapshotRef = useRef(getSnapshot);

  useEffect(() => {
    stateRef.current = state;
  }, [state]);

  useEffect(() => {
    projectIdRef.current = projectId;
  }, [projectId]);

  useEffect(() => {
    getSnapshotRef.current = getSnapshot;
  }, [getSnapshot]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   应用 event 到 React state，过滤非当前 project 的 session。
   *
   * Code Logic（这个函数做什么）:
   *   applyAgentRuntimeEvent；projectId 约束。
   */
  const applyEventToState = useCallback((event: AgentRuntimeEvent): void => {
    const pid = projectIdRef.current;
    if (pid && event.agentSession.projectId !== pid) {
      return;
    }
    setState((prev) => {
      const next = applyAgentRuntimeEvent(prev, event, 'live');
      stateRef.current = next;
      return next;
    });
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Gap/冷启动后用 snapshot 重建 baseline，再 drain 更大 sequence。
   *
   * Code Logic（这个函数做什么）:
   *   generation 防竞态；拉 snapshot → apply → 过滤缓冲 → drain → live。
   */
  const runHandshake = useCallback(async (): Promise<void> => {
    const generation = ++handshakeGenerationRef.current;
    phaseRef.current = 'pending';
    setPhase('pending');

    const pid = projectIdRef.current;
    if (!pid) {
      setState(emptyAgentRuntimeState());
      stateRef.current = emptyAgentRuntimeState();
      setError(null);
      return;
    }

    let snapshot: AgentRuntimeSnapshot;
    try {
      const raw = await getSnapshotRef.current(pid);
      snapshot = agentRuntimeSnapshotDecoder.decode(raw);
    } catch (err) {
      if (generation !== handshakeGenerationRef.current) return;
      // snapshot 失败：不得永久停在 pending（否则 live 事件无限缓冲、无法 re-handshake）。
      // 进入 error：保留 display-only cache 并标 offline；调用 refresh 可重试。
      setState((prev) => {
        if (prev.byAgentId.size === 0) {
          const empty = emptyAgentRuntimeState();
          stateRef.current = empty;
          return empty;
        }
        const marked = markAgentRuntimeFreshness(prev, 'offline');
        stateRef.current = marked;
        return marked;
      });
      setError(err instanceof Error ? err : new Error(String(err)));
      phaseRef.current = 'error';
      setPhase('error');
      return;
    }

    if (generation !== handshakeGenerationRef.current) return;
    if (projectIdRef.current !== pid) return;

    const nextState = applyAgentRuntimeSnapshot(snapshot, 'live');
    setState(nextState);
    stateRef.current = nextState;
    setError(null);
    cursorRef.current = {
      ownerInstanceId: snapshot.ownerInstanceId,
      sequence: snapshot.asOfSequence,
    };

    const asOfOwner = snapshot.ownerInstanceId;
    const asOfSeq = snapshot.asOfSequence;
    let highWater = asOfSeq;

    /**
     * Business Logic（为什么需要这个函数）:
     *   将缓冲事件按 asOf 过滤后同步 apply，并抬高 stream 高水位。
     *
     * Code Logic（这个函数做什么）:
     *   丢弃同 owner seq<=asOf 与异 owner；其余 applyEventToState 并更新 highWater。
     */
    const drainBatch = (batch: AgentRuntimeEvent[]): void => {
      for (const event of sortBufferedEvents(batch)) {
        if (generation !== handshakeGenerationRef.current) return;
        const owner = event.ownerInstanceId;
        const seq = event.sequence;
        if (
          typeof owner === 'string' &&
          owner === asOfOwner &&
          typeof seq === 'number' &&
          seq <= asOfSeq
        ) {
          continue;
        }
        if (typeof owner === 'string' && owner !== asOfOwner) {
          continue;
        }
        if (
          typeof owner === 'string' &&
          owner === asOfOwner &&
          typeof seq === 'number'
        ) {
          highWater = Math.max(highWater, seq);
        }
        applyEventToState(event);
      }
    };

    // Agent drain 同步；仍循环清空 buffer，并在进 live 后冲刷 residual
    const initial = bufferRef.current;
    bufferRef.current = [];
    drainBatch(initial);
    while (generation === handshakeGenerationRef.current && bufferRef.current.length > 0) {
      const more = bufferRef.current;
      bufferRef.current = [];
      drainBatch(more);
    }

    if (generation !== handshakeGenerationRef.current) return;
    cursorRef.current = {
      ownerInstanceId: asOfOwner,
      sequence: highWater,
    };
    phaseRef.current = 'live';
    setPhase('live');
    const residual = bufferRef.current;
    bufferRef.current = [];
    if (residual.length > 0) {
      drainBatch(residual);
      cursorRef.current = {
        ownerInstanceId: asOfOwner,
        sequence: highWater,
      };
    }
  }, [applyEventToState]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   live 到达时 handshake 未完成则缓冲；owner 变更触发 re-handshake。
   *
   * Code Logic（这个函数做什么）:
   *   pending → buffer；owner mismatch → re-handshake；过时 sequence 丢弃；否则 apply。
   */
  const handleLiveEvent = useCallback(
    (raw: unknown): void => {
      const event = normalizeAgentRuntimeEvent(raw);
      if (!event) return;

      if (phaseRef.current === 'pending') {
        bufferRef.current.push(event);
        return;
      }

      // snapshot 失败后的 error 态：缓冲并触发 re-handshake，避免永久离线
      if (phaseRef.current === 'error') {
        bufferRef.current.push(event);
        void runHandshake();
        return;
      }

      const cursor = cursorRef.current;
      if (
        cursor &&
        typeof event.ownerInstanceId === 'string' &&
        event.ownerInstanceId !== cursor.ownerInstanceId
      ) {
        bufferRef.current.push(event);
        void runHandshake();
        return;
      }

      if (
        cursor &&
        typeof event.ownerInstanceId === 'string' &&
        event.ownerInstanceId === cursor.ownerInstanceId &&
        typeof event.sequence === 'number' &&
        event.sequence <= cursor.sequence
      ) {
        return;
      }

      if (typeof event.sequence === 'number' && cursor) {
        cursorRef.current = {
          ownerInstanceId: event.ownerInstanceId ?? cursor.ownerInstanceId,
          sequence: Math.max(cursor.sequence, event.sequence),
        };
      }

      applyEventToState(event);
    },
    [applyEventToState, runHandshake],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   N1 gap 表示可能丢事件，必须暂停 apply 并重 baseline。
   *
   * Code Logic（这个函数做什么）:
   *   phase=pending；保留 buffer；runHandshake。
   */
  const handleGap = useCallback((): void => {
    phaseRef.current = 'pending';
    setPhase('pending');
    void runHandshake();
  }, [runHandshake]);

  useEffect(() => {
    if (!enabled || !projectId) {
      // React state 已在 render 中按 key 复位；此处仅清理异步 handshake 与 refs
      handshakeGenerationRef.current += 1;
      bufferRef.current = [];
      cursorRef.current = null;
      phaseRef.current = 'pending';
      stateRef.current = emptyAgentRuntimeState();
      return undefined;
    }

    let cancelled = false;
    let runtimeUnlisten: UnlistenFn | null = null;
    let gapUnlisten: UnlistenFn | null = null;

    const start = async (): Promise<void> => {
      if (canListenToTauriEvents()) {
        try {
          const unlistenRuntime = await listen<unknown>(WORKBENCH_AGENT_RUNTIME_EVENT, (ev) => {
            handleLiveEvent(ev.payload);
          });
          if (cancelled) {
            unlistenRuntime();
            return;
          }
          runtimeUnlisten = unlistenRuntime;

          const unlistenGap = await listen<unknown>(BACKEND_RUNTIME_GAP_EVENT, () => {
            handleGap();
          });
          if (cancelled) {
            unlistenGap();
            runtimeUnlisten?.();
            runtimeUnlisten = null;
            return;
          }
          gapUnlisten = unlistenGap;
        } catch {
          // listener 失败仍尝试 snapshot
        }
      }

      if (cancelled) return;
      await runHandshake();
    };

    void start();

    return () => {
      cancelled = true;
      handshakeGenerationRef.current += 1;
      if (runtimeUnlisten) runtimeUnlisten();
      if (gapUnlisten) gapUnlisten();
    };
  }, [enabled, projectId, handleGap, handleLiveEvent, runHandshake]);

  const latestAgentForTerminal = useCallback(
    (terminalSessionId: string): AgentSessionProjection | null =>
      selectLatestAgentForTerminal(state, terminalSessionId),
    [state],
  );

  const refresh = useCallback(async (): Promise<void> => {
    await runHandshake();
  }, [runHandshake]);

  return {
    state,
    phase,
    error,
    latestAgentForTerminal,
    refresh,
  };
}
