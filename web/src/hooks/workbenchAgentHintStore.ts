/**
 * 工作台 Agent hint 模块 store。
 *
 * Business Logic（为什么需要这个模块）:
 *   侧栏、worktree、窗口 tab 与 focus ack 必须共享同一份 window 级等待/完成真值，
 *   且不能塞进第 8 个 Workbench controller。
 *
 * Code Logic（这个模块做什么）:
 *   持有 immutable hint state + session worktree 索引；apply/ack/snapshot 后通知订阅者并 persist。
 */

import type { AgentSessionRuntimeDto } from '@/lib/types/agentRuntime';
import {
  ACKED_COMPLETED_STORAGE_KEY,
  SEEN_COMPLETED_STORAGE_KEY,
  ackCompletedForTerminal as ackCompletedInState,
  applyAgentHintSession,
  collectSeenCompleted,
  emptyAgentHintState,
  hintsForProject as selectProject,
  hintsForTerminal as selectTerminal,
  hintsForWorktree as selectWorktree,
  loadPersistedHintExtras,
  replaceActiveWaitingFromSnapshot,
  restoreSeenCompleted,
  serializeAckedCompleted,
  serializeSeenCompleted,
  stateWithAcked,
  type AgentHintCounts,
  type WorkbenchAgentHintState,
} from '@/lib/workbenchAgentHints';

export interface AgentHintSessionIndexEntry {
  sessionId: string;
  projectId: string;
  worktreeId?: string | null;
}

export type WorkbenchAgentHintListener = () => void;

export interface WorkbenchAgentHintStore {
  getState: () => WorkbenchAgentHintState;
  getRevision: () => number;
  subscribe: (listener: WorkbenchAgentHintListener) => () => void;
  applySession: (dto: AgentSessionRuntimeDto) => void;
  replaceActiveWaiting: (sessions: readonly AgentSessionRuntimeDto[]) => void;
  ackCompletedForTerminal: (terminalSessionId: string) => void;
  upsertSessionIndex: (entry: AgentHintSessionIndexEntry) => void;
  hintsForProject: (projectId: string) => AgentHintCounts;
  hintsForWorktree: (projectId: string, worktreeId: string) => AgentHintCounts;
  hintsForTerminal: (terminalSessionId: string) => AgentHintCounts;
  hydrateFromStorage: () => void;
}

interface PersistAdapter {
  getItem: (key: string) => string | null;
  setItem: (key: string, value: string) => void;
}

function memoryPersist(): PersistAdapter {
  const map = new Map<string, string>();
  return {
    getItem: (key) => map.get(key) ?? null,
    setItem: (key, value) => {
      map.set(key, value);
    },
  };
}

function browserPersist(): PersistAdapter {
  if (typeof window === 'undefined' || !window.localStorage) return memoryPersist();
  return {
    getItem: (key) => {
      try {
        return window.localStorage.getItem(key);
      } catch {
        return null;
      }
    },
    setItem: (key, value) => {
      try {
        window.localStorage.setItem(key, value);
      } catch {
        // 隐私模式 / quota：hint 仍可在本进程存活。
      }
    },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试与生产共用同一套 apply/ack/persist，避免 hook 再复制规则。
 *
 * Code Logic（这个函数做什么）:
 *   创建带 listener 的 store；可选注入 persist。
 */
export function createWorkbenchAgentHintStore(
  options: { persist?: PersistAdapter } = {},
): WorkbenchAgentHintStore {
  const persist = options.persist ?? browserPersist();
  const listeners = new Set<WorkbenchAgentHintListener>();
  const sessionWorktreeByTerminal: Record<string, string | undefined> = {};
  let state: WorkbenchAgentHintState = emptyAgentHintState();
  let revision = 0;

  const notify = (): void => {
    listeners.forEach((listener) => listener());
  };

  const persistState = (): void => {
    persist.setItem(
      ACKED_COMPLETED_STORAGE_KEY,
      serializeAckedCompleted([...state.ackedCompletedIds]),
    );
    persist.setItem(SEEN_COMPLETED_STORAGE_KEY, serializeSeenCompleted(collectSeenCompleted(state)));
  };

  const setState = (next: WorkbenchAgentHintState): void => {
    state = next;
    revision += 1;
    persistState();
    notify();
  };

  const applyOptions = () => ({ sessionWorktreeByTerminal });

  const hydrateFromStorage = (): void => {
    const extras = loadPersistedHintExtras({
      [ACKED_COMPLETED_STORAGE_KEY]: persist.getItem(ACKED_COMPLETED_STORAGE_KEY) ?? undefined,
      [SEEN_COMPLETED_STORAGE_KEY]: persist.getItem(SEEN_COMPLETED_STORAGE_KEY) ?? undefined,
    });
    const withAck = stateWithAcked(extras.ackedCompletedIds);
    state = restoreSeenCompleted(withAck, extras.seenCompleted);
    revision += 1;
    notify();
  };

  return {
    getState: () => state,
    getRevision: () => revision,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    applySession: (dto) => {
      setState(applyAgentHintSession(state, dto, applyOptions()));
    },
    replaceActiveWaiting: (sessions) => {
      setState(replaceActiveWaitingFromSnapshot(state, sessions, applyOptions()));
    },
    ackCompletedForTerminal: (terminalSessionId) => {
      setState(ackCompletedInState(state, terminalSessionId));
    },
    upsertSessionIndex: (entry) => {
      sessionWorktreeByTerminal[entry.sessionId] =
        entry.worktreeId && entry.worktreeId !== '' ? entry.worktreeId : undefined;
    },
    hintsForProject: (projectId) => selectProject(state, projectId),
    hintsForWorktree: (projectId, worktreeId) => selectWorktree(state, projectId, worktreeId),
    hintsForTerminal: (terminalSessionId) => selectTerminal(state, terminalSessionId),
    hydrateFromStorage,
  };
}

let defaultStore: WorkbenchAgentHintStore | null = null;

/**
 * Business Logic（为什么需要这个函数）:
 *   Rail 与 focusSession 必须 ack 同一份 store。
 *
 * Code Logic（这个函数做什么）:
 *   懒创建进程内单例。
 */
export function getWorkbenchAgentHintStore(): WorkbenchAgentHintStore {
  if (!defaultStore) defaultStore = createWorkbenchAgentHintStore();
  return defaultStore;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要隔离 persist，避免污染单例。
 *
 * Code Logic（这个函数做什么）:
 *   替换或清空默认 store。
 */
export function resetWorkbenchAgentHintStoreForTests(
  store: WorkbenchAgentHintStore | null = null,
): void {
  defaultStore = store;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端 controller 激活 window 时只需 ack，不必持 React hook。
 *
 * Code Logic（这个函数做什么）:
 *   转发默认 store。
 */
export function ackCompletedForTerminal(terminalSessionId: string): void {
  getWorkbenchAgentHintStore().ackCompletedForTerminal(terminalSessionId);
}
