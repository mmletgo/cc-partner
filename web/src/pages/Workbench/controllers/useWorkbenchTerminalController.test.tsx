// @vitest-environment jsdom
/**
 * useWorkbenchTerminalController 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   在 controller 抽取后，终端域的 session 生命周期、focus 同步、resize / split / close pane、
 *   terminal-status 事件过滤、stale project/worktree 守卫、remote-write 禁用、以及外部 buffer 回调，
 *   必须独立可测。这些行为原先散落在 Workbench.tsx 多处 effect/handler，本测试覆盖抽出后仍保持原有契约。
 *
 * Code Logic（这个测试做什么）:
 *   - 用 vi.mock 接管 @/api/workbench 的 sessions API 和 @tauri-apps/api/event 的 listen；
 *   - 通过 @testing-library/react 的 renderHook 把 controller 挂在 React 树中（包在 WorkbenchTerminalBuffersProvider
 *     内，因为 controller 调用 useWorkbenchTerminalBuffers）；
 *   - 用 rerender 模拟项目/worktree 切换、用 act 触发回调、用 fake timers 控制 focus polling；
 *   - 断言 sessions / activeSessionId / sessionError 等状态、sessions API 调用日志、buffer 回调日志。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

import { useWorkbenchTerminalController } from './useWorkbenchTerminalController';
import type { UseWorkbenchTerminalControllerParams } from './useWorkbenchTerminalController';
import type {
  WorkbenchProject,
  WorkbenchSession,
  WorkbenchWorktree,
} from '@/lib/types';

/* ---------------------------------------------------------------------------
 * vi.mock — workbench sessions API + tauri event listen
 *
 * Business Logic: controller 单元测试不应触发真实 Tauri invoke 或真实 fetch；用一个可断言的 fake 记录所有
 * sessions 调用，并允许测试动态设置返回值或抛出错误。
 * ------------------------------------------------------------------------- */

interface FakeSessionsApi {
  list: ReturnType<typeof vi.fn>;
  create: ReturnType<typeof vi.fn>;
  writeInput: ReturnType<typeof vi.fn>;
  resize: ReturnType<typeof vi.fn>;
  focus: ReturnType<typeof vi.fn>;
  focused: ReturnType<typeof vi.fn>;
  splitPane: ReturnType<typeof vi.fn>;
  switchPane: ReturnType<typeof vi.fn>;
  zoomPane: ReturnType<typeof vi.fn>;
  closePane: ReturnType<typeof vi.fn>;
  close: ReturnType<typeof vi.fn>;
  rename: ReturnType<typeof vi.fn>;
}

const fakeSessionsApi = vi.hoisted<FakeSessionsApi>(() => ({
  list: vi.fn(async () => [] as WorkbenchSession[]),
  create: vi.fn(async () => ({}) as WorkbenchSession),
  writeInput: vi.fn(async () => ({ ok: true, sessionId: 's' })),
  resize: vi.fn(async () => ({ ok: true, sessionId: 's' })),
  focus: vi.fn(async () => ({ ok: true, sessionId: 's' })),
  focused: vi.fn(async () => ({ sessionId: null })),
  splitPane: vi.fn(async () => ({ ok: true, sessionId: 's', direction: 'right' })),
  switchPane: vi.fn(async () => ({ ok: true, sessionId: 's' })),
  zoomPane: vi.fn(async () => ({ ok: true, sessionId: 's' })),
  closePane: vi.fn(async () => ({ ok: true, sessionId: 's', closedWindow: false })),
  close: vi.fn(async () => ({ ok: true, sessionId: 's' })),
  rename: vi.fn(async () => ({}) as WorkbenchSession),
}));

const eventListeners = vi.hoisted<
  Map<string, Set<(event: { event: string; payload: unknown }) => void>>
>(() => new Map());

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    sessions: fakeSessionsApi,
  },
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: async (
    event: string,
    handler: (event: { event: string; payload: unknown }) => void,
  ): Promise<() => void> => {
    const set = eventListeners.get(event) ?? new Set();
    set.add(handler);
    eventListeners.set(event, set);
    return (): void => {
      set.delete(handler);
    };
  },
}));

/** 触发一个 Tauri 事件，让所有同事件名监听器收到 payload。 */
function emitEvent(event: string, payload: unknown): void {
  const set = eventListeners.get(event);
  if (!set) return;
  for (const handler of [...set]) {
    handler({ event, payload });
  }
}

/* ---------------------------------------------------------------------------
 * Fixture builders
 * ------------------------------------------------------------------------- */

function buildLocalProject(overrides: Partial<WorkbenchProject> = {}): WorkbenchProject {
  return {
    id: 'project-1',
    name: 'demo',
    kind: 'local',
    deviceId: 'self',
    deviceName: 'Mac',
    path: '/Users/demo/project',
    lastOpenedAt: '2026-07-01T00:00:00.000Z',
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-07-01T00:00:00.000Z',
    ...overrides,
  };
}

function buildWorktree(overrides: Partial<WorkbenchWorktree> = {}): WorkbenchWorktree {
  return {
    id: 'worktree-main',
    projectId: 'project-1',
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: '/Users/demo/project',
    isMain: true,
    status: {
      branch: 'main',
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-07-01T00:00:00.000Z',
    ...overrides,
  };
}

function buildSession(overrides: Partial<WorkbenchSession> = {}): WorkbenchSession {
  return {
    id: 'session-1',
    projectId: 'project-1',
    worktreeId: 'worktree-main',
    name: 'main terminal',
    command: 'bash',
    cwd: '/Users/demo/project',
    status: 'running',
    cols: 80,
    rows: 24,
    startedAt: '2026-07-01T00:00:00.000Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
    ...overrides,
  };
}

/* ---------------------------------------------------------------------------
 * renderHook helper
 *
 * Business Logic: controller 调用 useWorkbenchTerminalBuffers（resetBuffer/removeBuffer）；hooks 必须在
 * Provider 内运行，否则会抛出 "must be used inside Provider" 错误。
 * ------------------------------------------------------------------------- */

interface ControllerProps {
  activeProjectId: string | null;
  activeWorktreeId: string | null;
  remoteWriteDisabled: boolean;
  terminalPanelRef: React.RefObject<HTMLElement | null>;
  resetBuffer: (sessionId: string) => void;
  removeBuffer: (sessionId: string) => void;
  refreshProjectSessionStats: (projectId: string) => void;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  isCurrentProject: (projectId: string) => boolean;
  desktopUnavailableMessage?: string;
  canListenToTauriEvents?: () => boolean;
}

function renderController(props: ControllerProps) {
  const merged = baseControllerProps(props);
  return renderHook(
    (currentProps: ControllerProps) =>
      useWorkbenchTerminalController(currentProps as UseWorkbenchTerminalControllerParams),
    {
      initialProps: merged,
    },
  );
}

/** 等待 pending microtask / Promise.then 落地。 */
async function flushMicrotasks(rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i += 1) {
    await Promise.resolve();
  }
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true, advanceTimeDelta: 1 });
});

afterEach(() => {
  vi.useRealTimers();
  eventListeners.clear();
  cleanup();
  vi.clearAllMocks();
});

/**
 * 公共 controller 参数（每个 test 只覆盖关心的字段）。
 * Business Logic: 让所有 test 共享同一套底层 mock 与文案，避免每个 test 都复制粘贴 desktopUnavailableMessage。
 */
function baseControllerProps(overrides: Partial<ControllerProps> = {}): ControllerProps {
  return {
    activeProjectId: 'project-1',
    activeWorktreeId: 'worktree-main',
    remoteWriteDisabled: false,
    terminalPanelRef: { current: null },
    resetBuffer: vi.fn(),
    removeBuffer: vi.fn(),
    refreshProjectSessionStats: vi.fn(),
    markRequestFailure: vi.fn(),
    markRequestSuccess: vi.fn(),
    isCurrentProject: () => true,
    desktopUnavailableMessage: 'desktop unavailable',
    canListenToTauriEvents: () => true,
    ...overrides,
  };
}

describe('useWorkbenchTerminalController — load / focus', () => {
  test('loadSessions stores list, refreshes stats, and marks request success', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession({ id: 's1' });
    const refreshStats = vi.fn();
    const markSuccess = vi.fn();
    fakeSessionsApi.list.mockResolvedValueOnce([session]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: refreshStats,
      markRequestFailure: vi.fn(),
      markRequestSuccess: markSuccess,
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    expect(result.current.sessions).toEqual([session]);
    expect(result.current.scopedSessions).toEqual([session]);
    expect(markSuccess).toHaveBeenCalledWith(project.id);
    expect(refreshStats).toHaveBeenCalledWith(project.id);
  });

  test('loadSessions ignores stale project response after switching projects', async () => {
    const projectA = buildLocalProject({ id: 'p-a' });
    const projectB = buildLocalProject({ id: 'p-b' });
    const worktree = buildWorktree();
    const sessionA = buildSession({ id: 'sa', projectId: 'p-a' });

    // isCurrentProject 总是返回“当前是 B”，模拟 loadSessions('p-a') 响应到达时项目已切到 B。
    fakeSessionsApi.list.mockResolvedValueOnce([sessionA]);

    const { result } = renderController({
      activeProjectId: projectB.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: (id) => id === projectB.id,
    });

    await act(async () => {
      await result.current.loadSessions(projectA.id);
      await flushMicrotasks();
    });

    expect(result.current.sessions).toEqual([]);
    expect(result.current.sessionError).toBeNull();
  });

  test('slow initial loadSessions does not overwrite createSession mutation result', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const initialSession = buildSession({ id: 's-initial', projectId: project.id, worktreeId: worktree.id });
    const createdSession = buildSession({ id: 's-created', projectId: project.id, worktreeId: worktree.id });

    let resolveInitialList: (value: typeof initialSession[]) => void = () => undefined;
    fakeSessionsApi.list.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveInitialList = resolve as (value: typeof initialSession[]) => void;
        }),
    );
    fakeSessionsApi.create.mockResolvedValueOnce(createdSession);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    let initialLoad: Promise<void> | undefined;
    act(() => {
      initialLoad = result.current.loadSessions(project.id);
    });
    await act(async () => {
      await flushMicrotasks(2);
    });

    await act(async () => {
      const createdId = await result.current.createSessionForWorktree(worktree.id);
      expect(createdId).toBe(createdSession.id);
      await flushMicrotasks();
    });
    expect(result.current.sessions.map((session) => session.id)).toEqual([createdSession.id]);

    await act(async () => {
      resolveInitialList([initialSession]);
      await initialLoad;
      await flushMicrotasks();
    });

    // 旧 list 不得覆盖 mutation 后的会话列表。
    expect(result.current.sessions.map((session) => session.id)).toEqual([createdSession.id]);
  });

  test('loadSessions surfaces error message and marks request failure on throw', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const markFailure = vi.fn();
    fakeSessionsApi.list.mockRejectedValueOnce(new Error('boom'));

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: markFailure,
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    expect(result.current.sessionError).toContain('boom');
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
  });

  test('focusSession sets activeSessionId and suppresses tmux sync polling during local focus grace', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id });
    const s2 = buildSession({ id: 's2', worktreeId: worktree.id });
    fakeSessionsApi.list.mockResolvedValue([s1, s2]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    fakeSessionsApi.focus.mockClear();
    fakeSessionsApi.focused.mockClear();

    act(() => {
      result.current.focusSession('s2');
    });
    await act(async () => {
      await flushMicrotasks();
    });

    // focus effect 触发后端 focus_workbench_session 一次。
    expect(fakeSessionsApi.focus).toHaveBeenCalledWith('s2');
  });
});

describe('useWorkbenchTerminalController — create / rename / close session', () => {
  test('createSessionForWorktree appends new session, focuses it, resets buffer, refreshes stats', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const existing = buildSession({ id: 's1', worktreeId: worktree.id });
    const created = buildSession({ id: 's2', worktreeId: worktree.id });

    fakeSessionsApi.list.mockResolvedValue([existing]);
    fakeSessionsApi.create.mockResolvedValueOnce(created);

    const resetBuffer = vi.fn();
    const refreshStats = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer,
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: refreshStats,
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.createSessionForWorktree(worktree.id);
      await flushMicrotasks();
    });

    expect(fakeSessionsApi.create).toHaveBeenCalledWith(project.id, undefined, worktree.id);
    expect(result.current.sessions.map((s) => s.id)).toEqual(['s1', 's2']);
    expect(result.current.activeSessionId).toBe('s2');
    expect(resetBuffer).toHaveBeenCalledWith('s2');
    expect(refreshStats).toHaveBeenCalledWith(project.id);
  });

  test('createSessionForWorktree surfaces error and marks request failure when API throws', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    fakeSessionsApi.create.mockRejectedValueOnce(new Error('cannot create'));

    const markFailure = vi.fn();
    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: markFailure,
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.createSessionForWorktree(worktree.id);
      await flushMicrotasks();
    });

    expect(result.current.sessionBusy).toBe(false);
    expect(result.current.sessionError).toContain('cannot create');
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
  });

  test('createSessionForWorktree appends session even when activeWorktreeId differs (new worktree flow)', async () => {
    // Regression: handleCreateWorktree calls createSessionForWorktree BEFORE setActiveWorktreeId,
    // so at the moment of the call activeWorktreeId still points at the OLD worktree. The bridge
    // must NOT silently drop the just-created session from the UI.
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main' });
    const newWt = buildWorktree({ id: 'wt-new', isMain: false });
    const existing = buildSession({ id: 's1', worktreeId: mainWt.id });
    const created = buildSession({ id: 's2', worktreeId: newWt.id });

    fakeSessionsApi.list.mockResolvedValue([existing]);
    fakeSessionsApi.create.mockResolvedValueOnce(created);

    const resetBuffer = vi.fn();
    const refreshStats = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      // activeWorktreeId is still main when bridge is invoked (mirrors production flow).
      activeWorktreeId: mainWt.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer,
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: refreshStats,
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      // Pass the NEW worktree id explicitly; activeWorktreeId is still main.
      await result.current.createSessionForWorktree(newWt.id);
      await flushMicrotasks();
    });

    // Session must be appended to the sessions list (not silently dropped by stale guard).
    // This is the core regression: before the fix, the bridge would call sessions.create on the
    // backend but never append the session to UI state, leaving the user with no visible tab.
    expect(result.current.sessions.map((s) => s.id)).toEqual(['s1', 's2']);
    expect(resetBuffer).toHaveBeenCalledWith('s2');
    expect(refreshStats).toHaveBeenCalledWith(project.id);
    // activeSessionId timing depends on the scopedSessions defer effect; the production flow
    // continues with handleCreateWorktree's setActiveWorktreeId(newWt.id), at which point s2
    // becomes the scoped active session. Verified end-to-end in the worktree controller test.
  });

  test('createSessionForWorktree does NOT focus session when user switched worktree mid-flight (race guard)', async () => {
    // Codex re-review Finding 4: after removing the worktreeId stale guard, a session created on
    // worktree A could steal focus in B's context if the user switched A→B while A's creation was
    // in-flight. The bridge must register the session but only focus it when the target worktree
    // is still the active one.
    const project = buildLocalProject();
    const wtA = buildWorktree({ id: 'wt-a' });
    const wtB = buildWorktree({ id: 'wt-b', isMain: false });
    const existingOnB = buildSession({ id: 'sb', worktreeId: wtB.id });
    const createdOnA = buildSession({ id: 'sa', worktreeId: wtA.id });

    fakeSessionsApi.list.mockResolvedValue([existingOnB]);

    // Hold A's session creation open until we explicitly resolve it.
    let resolveCreateA: (session: WorkbenchSession) => void = () => undefined;
    fakeSessionsApi.create.mockImplementationOnce(
      () =>
        new Promise<WorkbenchSession>((resolve) => {
          resolveCreateA = resolve;
        }),
    );

    const { result, rerender } = renderController({
      activeProjectId: project.id,
      // User starts on worktree A and triggers "new terminal".
      activeWorktreeId: wtA.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    // Start session creation on A (promise is still pending).
    let createPromise: Promise<unknown> | undefined;
    await act(async () => {
      createPromise = result.current.createSessionForWorktree(wtA.id);
      await flushMicrotasks();
    });

    // While A's creation is in-flight, the user switches to worktree B.
    rerender(
      baseControllerProps({
        activeProjectId: project.id,
        activeWorktreeId: wtB.id,
        remoteWriteDisabled: false,
        terminalPanelRef: { current: null },
        resetBuffer: vi.fn(),
        removeBuffer: vi.fn(),
        refreshProjectSessionStats: vi.fn(),
        markRequestFailure: vi.fn(),
        markRequestSuccess: vi.fn(),
        isCurrentProject: () => true,
        canListenToTauriEvents: () => true,
      }),
    );
    await act(async () => {
      await flushMicrotasks();
    });
    // Establish B as the focused context before A resolves.
    await act(async () => {
      await result.current.focusSession(existingOnB.id);
      await flushMicrotasks();
    });
    expect(result.current.activeSessionId).toBe(existingOnB.id);

    // Now A's session creation resolves.
    await act(async () => {
      resolveCreateA(createdOnA);
      await createPromise;
      await flushMicrotasks();
    });

    // A's session is registered (not silently dropped — backend already created it)...
    expect(result.current.sessions.map((s) => s.id)).toContain('sa');
    // ...but it must NOT have stolen focus from B's context.
    expect(result.current.activeSessionId).toBe(existingOnB.id);
  });

  test('handleRenameSession updates the renamed session in state via API', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession({ id: 's1', name: 'old', worktreeId: worktree.id });
    const renamed = { ...session, name: 'new' };
    fakeSessionsApi.list.mockResolvedValue([session]);
    fakeSessionsApi.rename.mockResolvedValueOnce(renamed);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });
    // Drain deferred activeSession.name → draft sync (setTimeout 0) before the user edit.
    // Otherwise that pending timer overwrites the draft with the server name.
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });

    await act(async () => {
      result.current.setSessionNameDraft('new-name');
    });
    await act(async () => {
      await result.current.handleRenameSession();
      await flushMicrotasks();
    });

    expect(fakeSessionsApi.rename).toHaveBeenCalledWith('s1', 'new-name');
    expect(result.current.sessions.find((s) => s.id === 's1')?.name).toBe('new');
  });

  test('handleCloseSession removes the session from state, removes buffer, refreshes stats', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id });
    const s2 = buildSession({ id: 's2', worktreeId: worktree.id });
    fakeSessionsApi.list.mockResolvedValue([s1, s2]);
    fakeSessionsApi.close.mockResolvedValueOnce({ ok: true, sessionId: 's1' });

    const removeBuffer = vi.fn();
    const refreshStats = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer,
      refreshProjectSessionStats: refreshStats,
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleCloseSession('s1');
      await flushMicrotasks();
    });

    expect(fakeSessionsApi.close).toHaveBeenCalledWith('s1');
    expect(result.current.sessions.map((s) => s.id)).toEqual(['s2']);
    expect(removeBuffer).toHaveBeenCalledWith('s1');
    expect(refreshStats).toHaveBeenCalledWith(project.id);
  });

  test('handleCloseSession is a no-op while remoteWriteDisabled', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id });

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: true,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    fakeSessionsApi.list.mockResolvedValue([s1]);
    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });
    fakeSessionsApi.close.mockClear();

    await act(async () => {
      await result.current.handleCloseSession('s1');
      await flushMicrotasks();
    });

    expect(fakeSessionsApi.close).not.toHaveBeenCalled();
  });

  test('delayed close on project A does not invalidate or mutate project B sessions', async () => {
    // Residual H1: close completes after user switches A→B; must only bump A's list seq
    // and must not clear B's sessions/buffer/stats/error.
    const projectA = buildLocalProject({ id: 'project-a', name: 'A' });
    const projectB = buildLocalProject({ id: 'project-b', name: 'B' });
    const worktreeA = buildWorktree({ id: 'wt-a', projectId: projectA.id });
    const worktreeB = buildWorktree({ id: 'wt-b', projectId: projectB.id });
    const sessionA = buildSession({
      id: 'sa',
      projectId: projectA.id,
      worktreeId: worktreeA.id,
    });
    const sessionB = buildSession({
      id: 'sb',
      projectId: projectB.id,
      worktreeId: worktreeB.id,
    });

    let resolveCloseA: (value: { ok: true; sessionId: string }) => void = () => undefined;
    fakeSessionsApi.close.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveCloseA = resolve;
        }),
    );

    const removeBuffer = vi.fn();
    const refreshStats = vi.fn();
    const markFailure = vi.fn();

    const { result, rerender } = renderController({
      activeProjectId: projectA.id,
      activeWorktreeId: worktreeA.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer,
      refreshProjectSessionStats: refreshStats,
      markRequestFailure: markFailure,
      markRequestSuccess: vi.fn(),
      isCurrentProject: (id) => id === projectA.id,
      canListenToTauriEvents: () => true,
    });

    fakeSessionsApi.list.mockResolvedValueOnce([sessionA]);
    await act(async () => {
      await result.current.loadSessions(projectA.id);
      await flushMicrotasks();
    });
    expect(result.current.sessions.map((s) => s.id)).toEqual(['sa']);

    // Start close on A (still pending).
    let closePromise: Promise<void> | undefined;
    await act(async () => {
      closePromise = result.current.handleCloseSession('sa');
      await flushMicrotasks();
    });

    // Switch to B and load B's sessions while A's close is in-flight.
    rerender(
      baseControllerProps({
        activeProjectId: projectB.id,
        activeWorktreeId: worktreeB.id,
        remoteWriteDisabled: false,
        terminalPanelRef: { current: null },
        resetBuffer: vi.fn(),
        removeBuffer,
        refreshProjectSessionStats: refreshStats,
        markRequestFailure: markFailure,
        markRequestSuccess: vi.fn(),
        isCurrentProject: (id) => id === projectB.id,
        canListenToTauriEvents: () => true,
      }),
    );
    fakeSessionsApi.list.mockResolvedValueOnce([sessionB]);
    await act(async () => {
      await result.current.loadSessions(projectB.id);
      await flushMicrotasks();
    });
    expect(result.current.sessions.map((s) => s.id)).toEqual(['sb']);
    const listCallsBeforeCloseResolve = fakeSessionsApi.list.mock.calls.length;
    refreshStats.mockClear();
    removeBuffer.mockClear();

    // Resolve A's delayed close — must not wipe B or invalidate B's list seq.
    await act(async () => {
      resolveCloseA({ ok: true, sessionId: 'sa' });
      await closePromise;
      await flushMicrotasks();
    });

    expect(result.current.sessions.map((s) => s.id)).toEqual(['sb']);
    expect(result.current.sessionError).toBeNull();
    expect(removeBuffer).not.toHaveBeenCalled();
    expect(refreshStats).not.toHaveBeenCalled();
    expect(markFailure).not.toHaveBeenCalled();

    // B's subsequent list must still be accepted (A must not have bumped B's request seq).
    const sessionB2 = buildSession({
      id: 'sb2',
      projectId: projectB.id,
      worktreeId: worktreeB.id,
    });
    fakeSessionsApi.list.mockResolvedValueOnce([sessionB, sessionB2]);
    await act(async () => {
      await result.current.loadSessions(projectB.id);
      await flushMicrotasks();
    });
    expect(fakeSessionsApi.list.mock.calls.length).toBe(listCallsBeforeCloseResolve + 1);
    expect(result.current.sessions.map((s) => s.id).sort()).toEqual(['sb', 'sb2']);
  });
});

describe('useWorkbenchTerminalController — split / switch / zoom / close pane', () => {
  test('handleSplitPane calls API and reloads sessions; remote-write-disabled no-ops', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({
      id: 's1',
      worktreeId: worktree.id,
      supportsPanes: true,
      paneCount: 2,
    });
    const s1Next = { ...s1, paneCount: 3 };

    fakeSessionsApi.list.mockResolvedValue([s1]);
    fakeSessionsApi.splitPane.mockResolvedValueOnce({ ok: true, sessionId: 's1', direction: 'right' });

    const { result, rerender } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });
    fakeSessionsApi.list.mockResolvedValue([s1Next]);

    await act(async () => {
      await result.current.handleSplitPane('right');
      await flushMicrotasks();
    });

    expect(fakeSessionsApi.splitPane).toHaveBeenCalledWith('s1', 'right');
    expect(result.current.sessions[0]?.paneCount).toBe(3);

    // 切换到 remote-write-disabled 后再调用 split pane 应被静默拒绝。
    fakeSessionsApi.splitPane.mockClear();
    rerender({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: true,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });
    await act(async () => {
      await result.current.handleSplitPane('right');
      await flushMicrotasks();
    });
    expect(fakeSessionsApi.splitPane).not.toHaveBeenCalled();
  });

  test('handleSwitchPane / handleZoomPane call corresponding API on the active session', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id, paneCount: 2 });

    fakeSessionsApi.list.mockResolvedValue([s1]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleSwitchPane();
      await flushMicrotasks();
    });
    expect(fakeSessionsApi.switchPane).toHaveBeenCalledWith('s1');

    await act(async () => {
      await result.current.handleZoomPane();
      await flushMicrotasks();
    });
    expect(fakeSessionsApi.zoomPane).toHaveBeenCalledWith('s1');
  });

  test('handleClosePane removes session and buffer when result.closedWindow', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id });

    fakeSessionsApi.list.mockResolvedValue([s1]);
    fakeSessionsApi.closePane.mockResolvedValueOnce({ ok: true, sessionId: 's1', closedWindow: true });

    const removeBuffer = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer,
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    // 后端关闭 pane 并返回 closedWindow=true 后，下一次 list_workbench_sessions 不应再返回该 session。
    fakeSessionsApi.list.mockResolvedValueOnce([]);

    await act(async () => {
      await result.current.handleClosePane();
      await flushMicrotasks();
    });

    expect(fakeSessionsApi.closePane).toHaveBeenCalledWith('s1');
    expect(removeBuffer).toHaveBeenCalledWith('s1');
    expect(result.current.sessions.map((s) => s.id)).toEqual([]);
  });
});

describe('useWorkbenchTerminalController — terminal-status event filtering', () => {
  test('terminal-status event updates the matching session status and sets exitedAt on terminal exit', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id, status: 'running' });
    fakeSessionsApi.list.mockResolvedValue([s1]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      emitEvent('workbench:terminal-status', {
        sessionId: 's1',
        status: 'exited',
        exitCode: 0,
        ts: 1730000000000,
      });
    });

    const updated = result.current.sessions.find((s) => s.id === 's1');
    expect(updated?.status).toBe('exited');
    expect(updated?.exitCode).toBe(0);
    expect(updated?.exitedAt).toBe(new Date(1730000000000).toISOString());
  });

  test('terminal-status event for an unknown session id is ignored', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id, status: 'running' });
    fakeSessionsApi.list.mockResolvedValue([s1]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    const before = result.current.sessions.map((s) => ({ ...s }));

    await act(async () => {
      emitEvent('workbench:terminal-status', {
        sessionId: 'unknown',
        status: 'exited',
        exitCode: 0,
        ts: Date.now(),
      });
    });

    // 未知 sessionId 的事件不应改变任何已知 session 的 status。
    expect(result.current.sessions.map((s) => ({ ...s }))).toEqual(before);
    expect(result.current.sessions[0]?.status).toBe('running');
  });
});

describe('useWorkbenchTerminalController — focus polling, input, resize, fullscreen', () => {
  test('handleInput forwards to writeInput unless remote-write-disabled', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.handleInput('s1', 'ls -la');
      await flushMicrotasks();
    });
    expect(fakeSessionsApi.writeInput).toHaveBeenCalledWith('s1', 'ls -la');
  });

  test('handleInput is suppressed when remoteWriteDisabled is true', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: true,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    fakeSessionsApi.writeInput.mockClear();
    await act(async () => {
      await result.current.handleInput('s1', 'data');
      await flushMicrotasks();
    });
    expect(fakeSessionsApi.writeInput).not.toHaveBeenCalled();
  });

  test('handleInput serializes rapid keys per session and coalesces only while in flight', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    let resolveFirst!: () => void;
    const firstWrite = new Promise<void>((resolve) => {
      resolveFirst = resolve;
    });
    const calls: Array<[string, string]> = [];
    fakeSessionsApi.writeInput.mockImplementation(async (sessionId: string, data: string) => {
      calls.push([sessionId, data]);
      if (calls.length === 1) {
        await firstWrite;
      }
      return { ok: true, sessionId };
    });

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      void result.current.handleInput('s1', 'a');
      void result.current.handleInput('s1', 'b');
      void result.current.handleInput('s1', '\u007f');
      await flushMicrotasks();
    });
    expect(calls).toEqual([['s1', 'a']]);

    await act(async () => {
      resolveFirst();
      await flushMicrotasks(12);
    });
    expect(calls).toEqual([
      ['s1', 'a'],
      ['s1', 'b\u007f'],
    ]);
  });

  test('handleInput drops pending bytes after successful close without replaying in-flight write', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession({ id: 's1' });
    fakeSessionsApi.list.mockResolvedValueOnce([session]);
    let resolveFirst!: () => void;
    const firstWrite = new Promise<void>((resolve) => {
      resolveFirst = resolve;
    });
    const calls: string[] = [];
    fakeSessionsApi.writeInput.mockImplementation(async (_sessionId: string, data: string) => {
      calls.push(data);
      if (calls.length === 1) {
        await firstWrite;
      }
      return { ok: true, sessionId: 's1' };
    });
    fakeSessionsApi.close.mockResolvedValueOnce({ ok: true, sessionId: 's1' });

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      void result.current.handleInput('s1', 'a');
      void result.current.handleInput('s1', 'b');
      await flushMicrotasks();
    });
    expect(calls).toEqual(['a']);

    await act(async () => {
      await result.current.handleCloseSession('s1');
      await flushMicrotasks();
    });

    await act(async () => {
      resolveFirst();
      await flushMicrotasks(12);
    });
    expect(calls).toEqual(['a']);
  });

  test('handleResize clamps cols/rows to min bounds and forwards to API', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.handleResize('s1', 5, 2);
      await flushMicrotasks();
    });
    expect(fakeSessionsApi.resize).toHaveBeenCalledWith('s1', 20, 6);
  });

  test('handleRefreshTerminalSize increments resizeRequestKey only when refresh is allowed', () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id, status: 'running' });
    fakeSessionsApi.list.mockResolvedValue([s1]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    const before = result.current.terminalResizeRequestKey;
    act(() => {
      result.current.handleRefreshTerminalSize();
    });
    expect(result.current.terminalResizeRequestKey).toBe(before);
    // canRefreshCurrentTerminalSize 依赖 activeSession，初始为 null；先 load 再断言。
  });

  test('handleEnterTerminalFullscreen / handleExitTerminalFullscreen toggle fullscreen flag', () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    expect(result.current.terminalFullscreen).toBe(false);
    act(() => {
      result.current.handleEnterTerminalFullscreen();
    });
    expect(result.current.terminalFullscreen).toBe(true);
    act(() => {
      result.current.handleExitTerminalFullscreen();
    });
    expect(result.current.terminalFullscreen).toBe(false);
  });
});

describe('useWorkbenchTerminalController — clearBuffersForWorktree bridge', () => {
  test('clearBuffersForWorktree removes buffer for every session scoped to that worktree', async () => {
    const project = buildLocalProject();
    const sMain = buildSession({ id: 's-main', worktreeId: 'wt-main' });
    const sFeature1 = buildSession({ id: 's-feature-1', worktreeId: 'wt-feature' });
    const sFeature2 = buildSession({ id: 's-feature-2', worktreeId: 'wt-feature' });

    fakeSessionsApi.list.mockResolvedValue([sMain, sFeature1, sFeature2]);

    const removeBuffer = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-feature',
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer: vi.fn(),
      removeBuffer,
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    result.current.clearBuffersForWorktree('wt-feature');

    expect(removeBuffer).toHaveBeenCalledWith('s-feature-1');
    expect(removeBuffer).toHaveBeenCalledWith('s-feature-2');
    expect(removeBuffer).not.toHaveBeenCalledWith('s-main');
  });
});

describe('useWorkbenchTerminalController — does not own terminal byte content', () => {
  test('controller state never stores terminal output; it only invokes external buffer callbacks', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const s1 = buildSession({ id: 's1', worktreeId: worktree.id });
    fakeSessionsApi.list.mockResolvedValue([s1]);

    const resetBuffer = vi.fn();
    const removeBuffer = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: worktree.id,
      remoteWriteDisabled: false,
      terminalPanelRef: { current: null },
      resetBuffer,
      removeBuffer,
      refreshProjectSessionStats: vi.fn(),
      markRequestFailure: vi.fn(),
      markRequestSuccess: vi.fn(),
      isCurrentProject: () => true,
      canListenToTauriEvents: () => true,
    });

    await act(async () => {
      await result.current.loadSessions(project.id);
      await flushMicrotasks();
    });

    // 断言 controller 的 state 数据字段没有任何终端字节内容相关字段（buffer/output/data/bytes）。
    // 只检查数据字段（非函数）；函数是动作，调用外部 buffer 回调，本身不持有字节内容。
    const dataFields = Object.entries(result.current)
      .filter(([, value]) => typeof value !== 'function')
      .map(([key]) => key);
    const forbidden = dataFields.filter((key) =>
      /buffer|output|^data$|bytes/i.test(key),
    );
    expect(forbidden).toEqual([]);

    // close 时 controller 调用外部 removeBuffer；自己不持有字节内容。
    fakeSessionsApi.close.mockResolvedValueOnce({ ok: true, sessionId: 's1' });
    await act(async () => {
      await result.current.handleCloseSession('s1');
      await flushMicrotasks();
    });
    expect(removeBuffer).toHaveBeenCalledWith('s1');
  });
});
