/**
 * 全项目 Agent 等待/完成 hint handshake hook。
 *
 * Business Logic（为什么需要这个模块）:
 *   侧栏项目卡在未进入项目时也要显示等待/完成数字，必须 listener-first
 *   拉全量 snapshot(null)，且 completed 不能被 list_active 冲掉。
 *
 * Code Logic（这个模块做什么）:
 *   先 listen workbench:agent-runtime，再 getSnapshot(null)；Gap 重握手并保留 persist。
 */

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  BACKEND_RUNTIME_GAP_EVENT,
  WORKBENCH_AGENT_RUNTIME_EVENT,
  workbenchApi,
} from '@/api/workbench';
import { normalizeAgentRuntimeEvent } from '@/hooks/useAgentRuntime';
import { agentRuntimeSnapshotDecoder } from '@/lib/schemas/agentRuntime';
import type { AgentRuntimeEvent, AgentRuntimeSnapshot } from '@/lib/types/agentRuntime';
import type { AgentHintCounts } from '@/lib/workbenchAgentHints';
import {
  createWorkbenchAgentHintStore,
  getWorkbenchAgentHintStore,
  type AgentHintSessionIndexEntry,
  type WorkbenchAgentHintStore,
} from './workbenchAgentHintStore';

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
 *   缓冲事件需按 sequence 顺序 drain。
 *
 * Code Logic（这个函数做什么）:
 *   sequence 升序；缺失 sequence 排后。
 */
function sortBufferedEvents(events: AgentRuntimeEvent[]): AgentRuntimeEvent[] {
  return [...events].sort((a, b) => {
    const sa = typeof a.sequence === 'number' ? a.sequence : Number.MAX_SAFE_INTEGER;
    const sb = typeof b.sequence === 'number' ? b.sequence : Number.MAX_SAFE_INTEGER;
    return sa - sb;
  });
}

export interface UseWorkbenchAgentHintsResult {
  phase: HandshakePhase;
  error: Error | null;
  hintsForProject: (projectId: string) => AgentHintCounts;
  hintsForWorktree: (projectId: string, worktreeId: string) => AgentHintCounts;
  hintsForTerminal: (terminalSessionId: string) => AgentHintCounts;
  ackCompletedForTerminal: (terminalSessionId: string) => void;
  refresh: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   App 壳需要一份全项目 hint，供 Rail 与 Workbench tab 共用。
 *
 * Code Logic（这个 hook 做什么）:
 *   hydrate persist → listener-first snapshot(null) → drain；选择器读 store。
 */
export function useWorkbenchAgentHints(options?: {
  getSnapshot?: (projectId: string | null) => Promise<AgentRuntimeSnapshot>;
  listSessionInventory?: () => Promise<readonly AgentHintSessionIndexEntry[]>;
  store?: WorkbenchAgentHintStore;
  enabled?: boolean;
}): UseWorkbenchAgentHintsResult {
  const enabled = options?.enabled !== false;
  const store = options?.store ?? getWorkbenchAgentHintStore();
  const injectedGetSnapshot = options?.getSnapshot;
  const getSnapshot = useCallback(
    (pid: string | null) =>
      injectedGetSnapshot
        ? injectedGetSnapshot(pid)
        : workbenchApi.agentRuntime.getSnapshot(pid),
    [injectedGetSnapshot],
  );
  const injectedListSessionInventory = options?.listSessionInventory;
  const listSessionInventory = useCallback(async (): Promise<readonly AgentHintSessionIndexEntry[]> => {
    if (injectedListSessionInventory) return injectedListSessionInventory();
    const sessions = await workbenchApi.sessions.list();
    return sessions.map((session) => ({
      sessionId: session.id,
      projectId: session.projectId,
      worktreeId: session.worktreeId,
    }));
  }, [injectedListSessionInventory]);

  const revision = useSyncExternalStore(store.subscribe, store.getRevision, store.getRevision);

  const [phase, setPhase] = useState<HandshakePhase>('pending');
  const [error, setError] = useState<Error | null>(null);

  const phaseRef = useRef<HandshakePhase>('pending');
  const bufferRef = useRef<AgentRuntimeEvent[]>([]);
  const cursorRef = useRef<{ ownerInstanceId: string; sequence: number } | null>(null);
  const handshakeGenerationRef = useRef(0);
  const getSnapshotRef = useRef(getSnapshot);
  const listSessionInventoryRef = useRef(listSessionInventory);
  const storeRef = useRef(store);

  useEffect(() => {
    getSnapshotRef.current = getSnapshot;
  }, [getSnapshot]);

  useEffect(() => {
    listSessionInventoryRef.current = listSessionInventory;
  }, [listSessionInventory]);

  useEffect(() => {
    storeRef.current = store;
  }, [store]);

  const applyEventToStore = useCallback((event: AgentRuntimeEvent): void => {
    storeRef.current.applySession(event.agentSession);
  }, []);

  const runHandshake = useCallback(async (): Promise<void> => {
    const generation = ++handshakeGenerationRef.current;
    phaseRef.current = 'pending';
    setPhase('pending');
    storeRef.current.hydrateFromStorage();

    let snapshot: AgentRuntimeSnapshot;
    let sessionInventory: readonly AgentHintSessionIndexEntry[];
    try {
      const [raw, inventory] = await Promise.all([
        getSnapshotRef.current(null),
        listSessionInventoryRef.current(),
      ]);
      snapshot = agentRuntimeSnapshotDecoder.decode(raw);
      sessionInventory = inventory;
    } catch (err) {
      if (generation !== handshakeGenerationRef.current) return;
      setError(err instanceof Error ? err : new Error(String(err)));
      phaseRef.current = 'error';
      setPhase('error');
      return;
    }

    if (generation !== handshakeGenerationRef.current) return;
    storeRef.current.replaceActiveWaiting(snapshot.sessions);
    storeRef.current.reconcileSessionInventory(sessionInventory);
    setError(null);
    cursorRef.current = {
      ownerInstanceId: snapshot.ownerInstanceId,
      sequence: snapshot.asOfSequence,
    };

    const asOfOwner = snapshot.ownerInstanceId;
    const asOfSeq = snapshot.asOfSequence;
    let highWater = asOfSeq;

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
        if (typeof owner === 'string' && owner !== asOfOwner) continue;
        if (typeof owner === 'string' && owner === asOfOwner && typeof seq === 'number') {
          highWater = Math.max(highWater, seq);
        }
        applyEventToStore(event);
      }
    };

    const initial = bufferRef.current;
    bufferRef.current = [];
    drainBatch(initial);
    while (generation === handshakeGenerationRef.current && bufferRef.current.length > 0) {
      const more = bufferRef.current;
      bufferRef.current = [];
      drainBatch(more);
    }

    if (generation !== handshakeGenerationRef.current) return;
    cursorRef.current = { ownerInstanceId: asOfOwner, sequence: highWater };
    phaseRef.current = 'live';
    setPhase('live');
    const residual = bufferRef.current;
    bufferRef.current = [];
    if (residual.length > 0) drainBatch(residual);
  }, [applyEventToStore]);

  const handleLiveEvent = useCallback(
    (raw: unknown): void => {
      const event = normalizeAgentRuntimeEvent(raw);
      if (!event) return;
      if (phaseRef.current === 'pending' || phaseRef.current === 'error') {
        bufferRef.current.push(event);
        if (phaseRef.current === 'error') void runHandshake();
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
      applyEventToStore(event);
    },
    [applyEventToStore, runHandshake],
  );

  const handleGap = useCallback((): void => {
    phaseRef.current = 'pending';
    setPhase('pending');
    void runHandshake();
  }, [runHandshake]);

  useEffect(() => {
    if (!enabled) return undefined;
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
  }, [enabled, handleGap, handleLiveEvent, runHandshake]);

  const hintsForProject = useCallback(
    (projectId: string): AgentHintCounts => store.hintsForProject(projectId),
    [store, revision],
  );
  const hintsForWorktree = useCallback(
    (projectId: string, worktreeId: string): AgentHintCounts =>
      store.hintsForWorktree(projectId, worktreeId),
    [store, revision],
  );
  const hintsForTerminal = useCallback(
    (terminalSessionId: string): AgentHintCounts => store.hintsForTerminal(terminalSessionId),
    [store, revision],
  );
  const ackCompletedForTerminal = useCallback(
    (terminalSessionId: string): void => {
      store.ackCompletedForTerminal(terminalSessionId);
    },
    [store],
  );
  const refresh = useCallback(async (): Promise<void> => {
    await runHandshake();
  }, [runHandshake]);

  return {
    phase,
    error,
    hintsForProject,
    hintsForWorktree,
    hintsForTerminal,
    ackCompletedForTerminal,
    refresh,
  };
}

export { createWorkbenchAgentHintStore };
