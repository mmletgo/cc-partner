// @vitest-environment jsdom
/**
 * useWorkbenchWorktreeGitController 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   在 controller 抽取后，worktree/Git 域的生命周期（load/select/create/remove/commit/push/merge）、
 *   创建表单 busy/error、Git 提交历史刷新、merge-progress 事件过滤（仅当前项目）以及 stale project/worktree
 *   守卫必须独立可测。这些行为原先散落在 Workbench.tsx 多处 state/effect/handler，本测试覆盖抽出后仍保持
 *   原有契约：
 *     - create worktree 通过 terminalBridge.createSessionForWorktree 创建 session；
 *     - remove/merge 使用显式 buffer/session bridge（terminalBridge.loadSessions / clearBuffersForWorktree），
 *       绝不直接 mutate terminal state；
 *     - 保留原有调用顺序与错误文案。
 *
 * Code Logic（这个测试做什么）:
 *   - 用 vi.mock 接管 @/api/workbench 的 worktrees/git API 和 @tauri-apps/api/event 的 listen；
 *   - 通过 @testing-library/react 的 renderHook 把 controller 挂在 React 树中；
 *   - 用 rerender 模拟项目/worktree 切换；用 act 触发回调；用 fake timers 控制 merge stage 自动隐藏；
 *   - 断言 worktrees / activeWorktreeId / worktreeBusy / worktreeError / gitCommits / mergeStages 等状态、
 *     worktrees/git API 调用日志、terminalBridge 回调日志。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { useEffect, useState } from 'react';

import { useWorkbenchWorktreeGitController } from './useWorkbenchWorktreeGitController';
import type { WorkbenchWorktreeGitErrorKey } from './useWorkbenchWorktreeGitController';
import type {
  WorkbenchGitCommit,
  WorkbenchHookFailure,
  WorkbenchMergeResult,
  WorkbenchProject,
  WorkbenchWorktree,
} from '@/lib/types';

/* ---------------------------------------------------------------------------
 * vi.mock — workbench worktrees/git API + tauri event listen
 *
 * Business Logic: controller 单元测试不应触发真实 Tauri invoke；用一个可断言的 fake 记录所有
 * worktrees/git 调用，并允许测试动态设置返回值或抛出错误。
 * ------------------------------------------------------------------------- */

interface FakeWorktreesApi {
  list: ReturnType<typeof vi.fn>;
  create: ReturnType<typeof vi.fn>;
  commit: ReturnType<typeof vi.fn>;
  push: ReturnType<typeof vi.fn>;
  merge: ReturnType<typeof vi.fn>;
  remove: ReturnType<typeof vi.fn>;
  getMutationOperation: ReturnType<typeof vi.fn>;
  repairHookFailure: ReturnType<typeof vi.fn>;
}

interface FakeGitApi {
  listCommits: ReturnType<typeof vi.fn>;
}

function succeededEnvelope<T>(value: T, clientOperationId = 'op-test') {
  return { kind: 'succeeded' as const, value, clientOperationId };
}

const fakeWorktreesApi = vi.hoisted<FakeWorktreesApi>(() => ({
  list: vi.fn(async () => [] as WorkbenchWorktree[]),
  create: vi.fn(async () => ({}) as WorkbenchWorktree),
  commit: vi.fn(async () =>
    succeededEnvelope({} as WorkbenchWorktree),
  ),
  push: vi.fn(async () =>
    succeededEnvelope({} as WorkbenchWorktree),
  ),
  merge: vi.fn(async () =>
    succeededEnvelope({ ok: true, worktreeId: 'wt', stages: [] } as WorkbenchMergeResult),
  ),
  remove: vi.fn(async () =>
    succeededEnvelope({ ok: true, worktreeId: 'wt' }),
  ),
  getMutationOperation: vi.fn(async () => null),
  repairHookFailure: vi.fn(async () => ({ agentSessionId: 'agent-1', terminalSessionId: 'term-1' })),
}));

const fakeGitApi = vi.hoisted<FakeGitApi>(() => ({
  listCommits: vi.fn(async () => [] as WorkbenchGitCommit[]),
}));

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    worktrees: fakeWorktreesApi,
    git: fakeGitApi,
  },
}));

const eventListeners = vi.hoisted<
  Map<string, Set<(event: { event: string; payload: unknown }) => void>>
>(() => new Map());

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
    id: 'wt-main',
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

function buildCommit(overrides: Partial<WorkbenchGitCommit> = {}): WorkbenchGitCommit {
  return {
    hash: 'abc1234',
    shortHash: 'abc1234',
    authorName: 'demo',
    authorEmail: 'demo@example.com',
    authoredAt: '2026-07-01T00:00:00.000Z',
    summary: 'init',
    parentHashes: [],
    refs: [],
    ...overrides,
  };
}

/* ---------------------------------------------------------------------------
 * renderHook helper + bridge fakes
 * ------------------------------------------------------------------------- */

interface TerminalBridgeFakes {
  loadSessions: ReturnType<typeof vi.fn>;
  focusSession: ReturnType<typeof vi.fn>;
  createSessionForWorktree: ReturnType<typeof vi.fn>;
  clearBuffersForWorktree: ReturnType<typeof vi.fn>;
}

interface ControllerProps {
  activeProjectId: string | null;
  activeWorktreeId: string | null;
  remoteWriteDisabled: boolean;
  inspectorTab: 'files' | 'history';
  isCurrentProject: (projectId: string) => boolean;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  refreshProjectSessionStats: (projectId: string) => void;
  displayErrorMessage: (error: unknown, fallback: string, desktopUnavailable: string) => string;
  desktopUnavailableMessage: string;
  canListenToTauriEvents: () => boolean;
  translateError: (key: string) => string;
  translateWorktreeMessage: (key: string, vars?: Record<string, unknown>) => string;
  confirmAction: (message: string) => boolean;
  /** 页面持有的 setActiveWorktreeId；测试用 stateful 实现记录最新值并触发 rerender。 */
  setActiveWorktreeId: (next: string | null) => void;
}

/**
 * 持有 activeWorktreeId 的测试 state；setActiveWorktreeId 触发 rerender 让 controller 读到最新值。
 * Business Logic: 页面持有 activeWorktreeId useState；测试模拟同一语义——controller 调用 setter 后，
 * 下一次 render controller 收到新的 activeWorktreeId prop。
 */
interface ActiveWorktreeState {
  value: string | null;
  setValue: (next: string | null) => void;
}

function buildBridgeFakes(): TerminalBridgeFakes {
  return {
    loadSessions: vi.fn(async () => undefined),
    focusSession: vi.fn(async () => true),
    // Returns a session id so the create-worktree orchestrator exercises the post-activation focus.
    createSessionForWorktree: vi.fn(async () => 'session-new'),
    clearBuffersForWorktree: vi.fn(),
  };
}

function baseControllerProps(
  overrides: Partial<ControllerProps> & { activeWorktreeId?: string | null } = {},
): ControllerProps {
  return {
    activeProjectId: 'project-1',
    activeWorktreeId: 'wt-main',
    remoteWriteDisabled: false,
    inspectorTab: 'files',
    isCurrentProject: () => true,
    markRequestFailure: vi.fn(),
    markRequestSuccess: vi.fn(),
    refreshProjectSessionStats: vi.fn(),
    displayErrorMessage: (error, fallback) => {
      const msg = error instanceof Error ? error.message : String(error);
      return msg && msg !== 'undefined' && msg !== 'null' ? msg : fallback;
    },
    desktopUnavailableMessage: 'desktop unavailable',
    canListenToTauriEvents: () => true,
    translateError: (key) => `err:${key}`,
    translateWorktreeMessage: (key, vars) =>
      vars && typeof vars === 'object' && 'name' in vars ? `${key}:${String(vars.name)}` : key,
    confirmAction: () => true,
    setActiveWorktreeId: () => undefined,
    ...overrides,
  };
}

function renderController(
  props: Partial<ControllerProps> & { activeWorktreeId?: string | null } = {},
  bridge: TerminalBridgeFakes = buildBridgeFakes(),
) {
  // 测试 state：模拟页面持有的 activeWorktreeId useState；controller 调用 setActiveWorktreeId 后，
  // holder 更新并通过 renderHook rerender 把新值传回 controller。
  const activeWorktreeState: ActiveWorktreeState = {
    value: props.activeWorktreeId ?? null,
    setValue: () => undefined,
  };
  const merged = baseControllerProps(props);

  const renderResult = renderHook(
    (currentProps: ControllerProps) => {
      const [activeWt, setActiveWt] = useStateInternal(currentProps.activeWorktreeId);
      // Business Logic: rerender 传入新 activeWorktreeId 时必须同步到 holder，否则 context-switch
      // 测试无法真正切换 worktree 上下文（useState 只吃初始值）。
      useEffect(() => {
        setActiveWt(currentProps.activeWorktreeId);
      }, [currentProps.activeWorktreeId]);
      activeWorktreeState.value = activeWt;
      activeWorktreeState.setValue = setActiveWt;
      // Business Logic: controller 调用 setActiveWorktreeId 时，既触发页面 state 更新（setActiveWt），
      // 也调用测试注入的 spy（currentProps.setActiveWorktreeId）便于断言。
      const handleSetActive = (next: string | null): void => {
        currentProps.setActiveWorktreeId(next);
        setActiveWt(next);
      };
      return useWorkbenchWorktreeGitController({
        activeProjectId: currentProps.activeProjectId,
        activeWorktreeId: activeWt,
        setActiveWorktreeId: handleSetActive,
        remoteWriteDisabled: currentProps.remoteWriteDisabled,
        inspectorTab: currentProps.inspectorTab,
        isCurrentProject: currentProps.isCurrentProject,
        markRequestFailure: currentProps.markRequestFailure,
        markRequestSuccess: currentProps.markRequestSuccess,
        refreshProjectSessionStats: currentProps.refreshProjectSessionStats,
        terminalBridge: {
          loadSessions: bridge.loadSessions as (projectId?: string) => Promise<void>,
          focusSession: bridge.focusSession as (sessionId: string) => Promise<boolean>,
          createSessionForWorktree: bridge.createSessionForWorktree as (
            worktreeId: string,
          ) => Promise<string | null>,
          clearBuffersForWorktree: bridge.clearBuffersForWorktree as (
            worktreeId: string,
          ) => void,
        },
        displayErrorMessage: currentProps.displayErrorMessage,
        desktopUnavailableMessage: currentProps.desktopUnavailableMessage,
        translateError: currentProps.translateError as (
          key: WorkbenchWorktreeGitErrorKey,
        ) => string,
        translateWorktreeMessage: currentProps.translateWorktreeMessage as (
          key: 'mergeConfirm' | 'removeConfirm' | 'checkSourceMessage',
          vars?: Record<string, unknown>,
        ) => string,
        confirmAction: currentProps.confirmAction,
        canListenToTauriEvents: currentProps.canListenToTauriEvents,
      });
    },
    { initialProps: merged },
  );

  return {
    bridge,
    activeWorktreeState,
    ...renderResult,
  };
}

/**
 * React useState 的薄封装，仅为避免与 controller 内部 state 命名冲突。
 * Business Logic: renderHook 回调在 React 树内运行，可合法调用 useState 持有测试 activeWorktreeId state。
 */
function useStateInternal(initial: string | null): [string | null, (next: string | null) => void] {
  return useState<string | null>(initial);
}

/** 等待 pending microtask / Promise.then 落地。 */
async function flushMicrotasks(rounds = 8): Promise<void> {
  for (let i = 0; i < rounds; i += 1) {
    await Promise.resolve();
  }
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true, advanceTimeDelta: 1 });
  // mockReset 清空 mockResolvedValueOnce 队列与实现，随后重设默认实现；clearAllMocks 只清调用记录。
  fakeWorktreesApi.list.mockReset();
  fakeWorktreesApi.create.mockReset();
  fakeWorktreesApi.commit.mockReset();
  fakeWorktreesApi.push.mockReset();
  fakeWorktreesApi.merge.mockReset();
  fakeWorktreesApi.remove.mockReset();
  fakeWorktreesApi.getMutationOperation.mockReset();
  fakeGitApi.listCommits.mockReset();
  fakeWorktreesApi.list.mockImplementation(async () => [] as WorkbenchWorktree[]);
  fakeWorktreesApi.create.mockImplementation(async () => ({}) as WorkbenchWorktree);
  fakeWorktreesApi.commit.mockImplementation(async () =>
    succeededEnvelope({} as WorkbenchWorktree),
  );
  fakeWorktreesApi.push.mockImplementation(async () =>
    succeededEnvelope({} as WorkbenchWorktree),
  );
  fakeWorktreesApi.merge.mockImplementation(async () =>
    succeededEnvelope({ ok: true, worktreeId: 'wt', stages: [] } as WorkbenchMergeResult),
  );
  fakeWorktreesApi.remove.mockImplementation(async () =>
    succeededEnvelope({ ok: true, worktreeId: 'wt' }),
  );
  fakeWorktreesApi.getMutationOperation.mockImplementation(async () => null);
  fakeGitApi.listCommits.mockImplementation(async () => [] as WorkbenchGitCommit[]);
});

afterEach(() => {
  vi.useRealTimers();
  eventListeners.clear();
  cleanup();
  vi.clearAllMocks();
});

/* ---------------------------------------------------------------------------
 * load / select
 * ------------------------------------------------------------------------- */

describe('useWorkbenchWorktreeGitController — load / select', () => {
  test('loadWorktrees stores list, picks first worktree as active when none selected, marks success', async () => {
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', isMain: true });
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false, branch: 'feature/feat' });
    fakeWorktreesApi.list.mockResolvedValueOnce([mainWt, featWt]);
    const markSuccess = vi.fn();
    const setActive = vi.fn();

    const { result, activeWorktreeState } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: null,
      markRequestSuccess: markSuccess,
      setActiveWorktreeId: setActive,
    });

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    expect(result.current.worktrees).toEqual([mainWt, featWt]);
    expect(activeWorktreeState.value).toBe('wt-main');
    expect(setActive).toHaveBeenCalledWith('wt-main');
    expect(result.current.worktreeError).toBeNull();
    expect(markSuccess).toHaveBeenCalledWith(project.id);
  });

  test('loadWorktrees keeps currently selected worktree if still present', async () => {
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', isMain: true });
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false });
    fakeWorktreesApi.list.mockResolvedValue([mainWt, featWt]);
    const setActive = vi.fn();

    const { result, activeWorktreeState } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-feat',
      setActiveWorktreeId: setActive,
    });

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    expect(activeWorktreeState.value).toBe('wt-feat');
    // setActiveWorktreeId 不应被调用，因为当前 active 仍存在于列表中。
    expect(setActive).not.toHaveBeenCalled();
  });

  test('loadWorktrees drops stale response after project switches', async () => {
    const projectA = buildLocalProject({ id: 'p-a' });
    const mainWtA = buildWorktree({ id: 'wt-a', projectId: 'p-a' });
    fakeWorktreesApi.list.mockResolvedValueOnce([mainWtA]);

    const { result } = renderController({
      activeProjectId: 'p-b', // isCurrentProject('p-a') -> false
      activeWorktreeId: null,
      isCurrentProject: (id) => id === 'p-b',
    });

    await act(async () => {
      await result.current.loadWorktrees(projectA.id);
      await flushMicrotasks();
    });

    expect(result.current.worktrees).toEqual([]);
  });

  test('slow initial loadWorktrees does not overwrite later refresh after mutation-triggered reload', async () => {
    const project = buildLocalProject();
    const initialList = [buildWorktree({ id: 'wt-old', projectId: project.id })];
    const refreshedList = [
      buildWorktree({ id: 'wt-old', projectId: project.id }),
      buildWorktree({ id: 'wt-new', projectId: project.id, isMain: false }),
    ];

    let resolveInitial: (value: typeof initialList) => void = () => undefined;
    fakeWorktreesApi.list
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveInitial = resolve as (value: typeof initialList) => void;
          }),
      )
      .mockResolvedValueOnce(refreshedList);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: null,
    });

    let firstLoad: Promise<WorkbenchWorktree[] | null> | undefined;
    act(() => {
      firstLoad = result.current.loadWorktrees(project.id);
    });
    await act(async () => {
      await flushMicrotasks(2);
    });

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });
    expect(result.current.worktrees.map((worktree) => worktree.id)).toEqual(['wt-old', 'wt-new']);

    await act(async () => {
      resolveInitial(initialList);
      await firstLoad;
      await flushMicrotasks();
    });

    expect(result.current.worktrees.map((worktree) => worktree.id)).toEqual(['wt-old', 'wt-new']);
  });

  test('loadWorktrees surfaces error and marks failure on throw', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.list.mockRejectedValueOnce(new Error('boom'));
    const markFailure = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: null,
      markRequestFailure: markFailure,
    });

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    expect(result.current.worktreeError).toContain('boom');
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
  });

  test('controller delegates active worktree selection to page setActiveWorktreeId', async () => {
    const mainWt = buildWorktree({ id: 'wt-main', isMain: true });
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false });
    fakeWorktreesApi.list.mockResolvedValueOnce([mainWt, featWt]);
    const setActive = vi.fn();

    const { result, activeWorktreeState } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: null,
      setActiveWorktreeId: setActive,
    });

    await act(async () => {
      await result.current.loadWorktrees('project-1');
      await flushMicrotasks();
    });

    // loadWorktrees 选中第一个 worktree，通过页面 setter 同步。
    expect(setActive).toHaveBeenCalledWith('wt-main');
    expect(activeWorktreeState.value).toBe('wt-main');
  });
});

/* ---------------------------------------------------------------------------
 * create form + create action
 * ------------------------------------------------------------------------- */

describe('useWorkbenchWorktreeGitController — create form / create action', () => {
  test('handleOpenCreateWorktree opens form with cleared draft when writes allowed and not busy', () => {
    const { result } = renderController({ activeProjectId: 'project-1', remoteWriteDisabled: false });

    act(() => {
      result.current.setCreateWorktreeBranchSuffixDraft('stale');
    });
    act(() => {
      result.current.handleOpenCreateWorktree();
    });

    expect(result.current.createWorktreeOpen).toBe(true);
    expect(result.current.createWorktreeBranchSuffixDraft).toBe('');
    expect(result.current.createWorktreeBranchPrefix).toBe('feature');
  });

  test('handleOpenCreateWorktree is a no-op when remoteWriteDisabled or busy', () => {
    const { result } = renderController({ activeProjectId: 'project-1', remoteWriteDisabled: true });

    act(() => {
      result.current.handleOpenCreateWorktree();
    });
    expect(result.current.createWorktreeOpen).toBe(false);
  });

  test('handleCancelCreateWorktree closes form and clears draft unless busy creating', () => {
    const { result } = renderController({ activeProjectId: 'project-1' });

    act(() => {
      result.current.handleOpenCreateWorktree();
    });
    act(() => {
      result.current.setCreateWorktreeBranchSuffixDraft('abc');
    });
    act(() => {
      result.current.handleCancelCreateWorktree();
    });

    expect(result.current.createWorktreeOpen).toBe(false);
    expect(result.current.createWorktreeBranchSuffixDraft).toBe('');
  });

  test('handleCreateWorktree creates worktree then invokes terminalBridge.createSessionForWorktree, reloads worktrees and selects new worktree', async () => {
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', isMain: true });
    const createdWt = buildWorktree({
      id: 'wt-new',
      isMain: false,
      branch: 'feature/feat',
      name: 'feat',
    });
    fakeWorktreesApi.create.mockResolvedValueOnce(createdWt);
    // loadWorktrees after create returns main + new.
    fakeWorktreesApi.list.mockResolvedValueOnce([mainWt, createdWt]);

    const bridge = buildBridgeFakes();
    const refreshStats = vi.fn();
    const setActive = vi.fn();

    const { result, activeWorktreeState } = renderController(
      {
        activeProjectId: project.id,
        activeWorktreeId: 'wt-main',
        refreshProjectSessionStats: refreshStats,
        setActiveWorktreeId: setActive,
      },
      bridge,
    );

    act(() => {
      result.current.handleOpenCreateWorktree();
    });
    act(() => {
      result.current.setCreateWorktreeBranchSuffixDraft('feat');
    });

    await act(async () => {
      await result.current.handleCreateWorktree();
      await flushMicrotasks();
    });

    // create worktree before createSessionForWorktree.
    const createWtCall = fakeWorktreesApi.create.mock.invocationCallOrder[0];
    const createSessionCall = bridge.createSessionForWorktree.mock.invocationCallOrder[0];
    expect(createSessionCall).toBeGreaterThan(createWtCall);
    expect(bridge.createSessionForWorktree).toHaveBeenCalledWith(createdWt.id);

    // form closed + draft cleared.
    expect(result.current.createWorktreeOpen).toBe(false);
    expect(result.current.createWorktreeBranchSuffixDraft).toBe('');
    // new worktree selected via page setter.
    expect(setActive).toHaveBeenCalledWith('wt-new');
    expect(activeWorktreeState.value).toBe('wt-new');
    expect(result.current.worktrees.map((w) => w.id)).toEqual(['wt-main', 'wt-new']);
    // busy cleared.
    expect(result.current.worktreeBusy).toBeNull();

    // Codex re-review Finding 4: focus on the new session happens AFTER setActiveWorktreeId (so the
    // focus lands in the correct, now-active worktree context), not inside the bridge call.
    expect(bridge.focusSession).toHaveBeenCalledWith('session-new');
    const setActiveCall = setActive.mock.invocationCallOrder[0];
    const focusCall = bridge.focusSession.mock.invocationCallOrder[0];
    expect(focusCall).toBeGreaterThan(setActiveCall);
  });

  test('handleCreateWorktree surfaces error and marks failure when worktree create throws', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.create.mockRejectedValueOnce(new Error('cannot create'));
    const markFailure = vi.fn();
    const bridge = buildBridgeFakes();

    const { result } = renderController(
      {
        activeProjectId: project.id,
        activeWorktreeId: 'wt-main',
        markRequestFailure: markFailure,
      },
      bridge,
    );

    act(() => {
      result.current.handleOpenCreateWorktree();
    });
    act(() => {
      result.current.setCreateWorktreeBranchSuffixDraft('feat');
    });

    await act(async () => {
      await result.current.handleCreateWorktree();
      await flushMicrotasks();
    });

    expect(result.current.worktreeError).toContain('cannot create');
    expect(result.current.worktreeBusy).toBeNull();
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
    // bridge.createSessionForWorktree must not run when worktree creation failed.
    expect(bridge.createSessionForWorktree).not.toHaveBeenCalled();
  });

  test('create success after same-project worktree switch does not steal active worktree', async () => {
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', isMain: true });
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false, name: 'feat' });
    const createdWt = buildWorktree({
      id: 'wt-new',
      isMain: false,
      branch: 'feature/late',
      name: 'late',
    });
    let settleCreate!: (value: WorkbenchWorktree) => void;
    fakeWorktreesApi.create.mockReturnValueOnce(
      new Promise<WorkbenchWorktree>((resolve) => {
        settleCreate = resolve;
      }),
    );
    fakeWorktreesApi.list.mockResolvedValue([mainWt, featWt, createdWt]);
    const setActive = vi.fn();
    const bridge = buildBridgeFakes();

    const { result, rerender } = renderController(
      {
        activeProjectId: project.id,
        activeWorktreeId: 'wt-main',
        setActiveWorktreeId: setActive,
      },
      bridge,
    );

    act(() => {
      result.current.handleOpenCreateWorktree();
      result.current.setCreateWorktreeBranchSuffixDraft('late');
    });

    let operation!: Promise<void>;
    await act(async () => {
      operation = result.current.handleCreateWorktree();
      await flushMicrotasks();
    });
    expect(result.current.worktreeBusy).toBe('create');

    // 同 project 内切到 feat；旧 create 不得 setActiveWorktreeId(created)。
    rerender(
      baseControllerProps({
        activeProjectId: project.id,
        activeWorktreeId: 'wt-feat',
        setActiveWorktreeId: setActive,
      }),
    );
    await act(async () => {
      await flushMicrotasks();
    });

    await act(async () => {
      settleCreate(createdWt);
      await operation;
      await flushMicrotasks();
    });

    expect(setActive).not.toHaveBeenCalledWith('wt-new');
    expect(bridge.focusSession).not.toHaveBeenCalled();
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('handleCreateWorktree is a no-op without active project or when remoteWriteDisabled', async () => {
    const bridge = buildBridgeFakes();
    const { result } = renderController(
      {
        activeProjectId: null,
        remoteWriteDisabled: false,
      },
      bridge,
    );

    await act(async () => {
      await result.current.handleCreateWorktree();
      await flushMicrotasks();
    });
    expect(fakeWorktreesApi.create).not.toHaveBeenCalled();
    expect(bridge.createSessionForWorktree).not.toHaveBeenCalled();
  });
});

/* ---------------------------------------------------------------------------
 * commit / push
 * ------------------------------------------------------------------------- */

describe('useWorkbenchWorktreeGitController — commit / push', () => {
  test('handleCommitWorktree commits, reloads worktrees, refreshes git history when on history tab', async () => {
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', projectId: project.id });
    const committed = buildWorktree({ id: 'wt-main', projectId: project.id, status: { ...mainWt.status, changed: 0 } });
    fakeWorktreesApi.commit.mockResolvedValueOnce(succeededEnvelope(committed));
    // loadWorktrees + nested loadGitHistory both list; keep a stable non-empty reconcile result.
    fakeWorktreesApi.list.mockResolvedValue([committed]);
    const commit = buildCommit({ hash: 'c1' });
    fakeGitApi.listCommits.mockResolvedValueOnce([commit]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      inspectorTab: 'history',
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    expect(fakeWorktreesApi.commit).toHaveBeenCalledWith(
      'wt-main',
      null,
      expect.any(String),
    );
    expect(fakeWorktreesApi.list).toHaveBeenCalledWith(project.id);
    expect(fakeGitApi.listCommits).toHaveBeenCalledWith(project.id, 'wt-main', 30);
    expect(result.current.gitCommits).toEqual([commit]);
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('handleCommitWorktree does not refresh git history when inspectorTab !== history', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.commit.mockResolvedValueOnce(
      succeededEnvelope(buildWorktree({ id: 'wt-main' })),
    );
    fakeWorktreesApi.list.mockResolvedValueOnce([buildWorktree({ id: 'wt-main' })]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      inspectorTab: 'files',
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    expect(fakeGitApi.listCommits).not.toHaveBeenCalled();
  });

  test('handleCommitWorktree surfaces error and still reloads worktrees + git history on failure', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.commit.mockRejectedValueOnce(new Error('commit failed'));
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' })]);
    const commit = buildCommit();
    fakeGitApi.listCommits.mockResolvedValueOnce([commit]);
    const markFailure = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      inspectorTab: 'history',
      markRequestFailure: markFailure,
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    expect(result.current.worktreeError).toContain('commit failed');
    expect(fakeWorktreesApi.list).toHaveBeenCalledWith(project.id);
    expect(fakeGitApi.listCommits).toHaveBeenCalled();
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('handlePushWorktree pushes, reloads worktrees, refreshes git history when on history tab', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.push.mockResolvedValueOnce(
      succeededEnvelope(buildWorktree({ id: 'wt-main' })),
    );
    fakeWorktreesApi.list.mockResolvedValueOnce([buildWorktree({ id: 'wt-main' })]);
    fakeGitApi.listCommits.mockResolvedValueOnce([buildCommit()]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      inspectorTab: 'history',
    });

    await act(async () => {
      await result.current.handlePushWorktree();
      await flushMicrotasks();
    });

    expect(fakeWorktreesApi.push).toHaveBeenCalledWith('wt-main', expect.any(String));
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('handlePushWorktree surfaces error and marks failure on throw', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.push.mockRejectedValueOnce(new Error('push failed'));
    const markFailure = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      inspectorTab: 'files',
      markRequestFailure: markFailure,
    });

    await act(async () => {
      await result.current.handlePushWorktree();
      await flushMicrotasks();
    });

    expect(result.current.worktreeError).toContain('push failed');
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('commit/push are no-ops when no active worktree or remoteWriteDisabled', async () => {
    const { result } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: null,
      remoteWriteDisabled: false,
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await result.current.handlePushWorktree();
      await flushMicrotasks();
    });
    expect(fakeWorktreesApi.commit).not.toHaveBeenCalled();
    expect(fakeWorktreesApi.push).not.toHaveBeenCalled();
  });

  test.each(['resolve', 'reject'] as const)(
    'old commit %s cannot mutate new context busy/error',
    async (outcome) => {
      const projectA = buildLocalProject({ id: 'project-A' });
      const projectB = buildLocalProject({ id: 'project-B' });
      let settle!: (value: unknown) => void;
      let reject!: (reason?: unknown) => void;
      const pending = new Promise((resolve, rej) => {
        settle = resolve;
        reject = rej;
      });
      fakeWorktreesApi.commit.mockReturnValueOnce(pending);

      const { result, rerender } = renderController({
        activeProjectId: projectA.id,
        activeWorktreeId: 'wt-a',
      });

      let operation!: Promise<void>;
      await act(async () => {
        operation = result.current.handleCommitWorktree();
        await flushMicrotasks();
      });
      expect(result.current.worktreeBusy).toBe('commit');

      // 切到 B 上下文；旧 settlement 不得写 B 的 busy/error。
      rerender({
        ...baseControllerProps({
          activeProjectId: projectB.id,
          activeWorktreeId: 'wt-b',
        }),
      });
      await act(async () => {
        await flushMicrotasks();
      });

      await act(async () => {
        if (outcome === 'resolve') {
          settle(succeededEnvelope(buildWorktree({ id: 'wt-a', projectId: projectA.id })));
        } else {
          reject(new Error('stale commit failed'));
        }
        await operation.catch(() => undefined);
        await flushMicrotasks();
      });

      expect(result.current.worktreeBusy).toBeNull();
      expect(result.current.worktreeError).toBeNull();
    },
  );

  test('unknown envelope sets mutationUnknown and queries ledger without blind replay', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.commit.mockResolvedValueOnce({
      kind: 'unknown',
      clientOperationId: 'op-unknown-1',
      transportClass: 'timeout',
    });
    fakeWorktreesApi.getMutationOperation.mockResolvedValueOnce({
      clientOperationId: 'op-unknown-1',
      kind: 'commit',
      payloadHash: 'h',
      intent: {
        kind: 'commit',
        projectId: project.id,
        worktreeId: 'wt-main',
        beforeHead: 'a',
        expectedTree: 't',
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: project.id,
      worktreeId: 'wt-main',
      createdAt: '2026-07-14T00:00:00.000Z',
      updatedAt: '2026-07-14T00:00:00.000Z',
    });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' })]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      inspectorTab: 'files',
      translateError: (key) =>
        key === 'mutationUnknown' ? '结果未知，请刷新后人工核对' : `err:${key}`,
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    expect(fakeWorktreesApi.getMutationOperation).toHaveBeenCalledWith('op-unknown-1');
    // 不得二次 commit（盲重放）。
    expect(fakeWorktreesApi.commit).toHaveBeenCalledTimes(1);
    expect(result.current.worktreeError).toContain('结果未知');
    expect(result.current.worktreeBusy).toBeNull();
    expect(result.current.unknownMutationLock).toEqual({
      kind: 'commit',
      projectId: project.id,
      worktreeId: 'wt-main',
      clientOperationId: 'op-unknown-1',
    });
  });

  test('second commit after unknown reuses same clientOperationId and only reconciles', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.commit.mockResolvedValueOnce({
      kind: 'unknown',
      clientOperationId: 'op-stable',
      transportClass: 'timeout',
    });
    fakeWorktreesApi.getMutationOperation
      .mockResolvedValueOnce({
        clientOperationId: 'op-stable',
        kind: 'commit',
        payloadHash: 'h',
        intent: {
          kind: 'commit',
          projectId: project.id,
          worktreeId: 'wt-main',
          beforeHead: 'a',
          expectedTree: 't',
        },
        state: 'running',
        outcome: null,
        errorMessage: null,
        projectId: project.id,
        worktreeId: 'wt-main',
        createdAt: '2026-07-14T00:00:00.000Z',
        updatedAt: '2026-07-14T00:00:00.000Z',
      })
      .mockResolvedValueOnce({
        clientOperationId: 'op-stable',
        kind: 'commit',
        payloadHash: 'h',
        intent: {
          kind: 'commit',
          projectId: project.id,
          worktreeId: 'wt-main',
          beforeHead: 'a',
          expectedTree: 't',
        },
        state: 'succeeded',
        outcome: null,
        errorMessage: null,
        projectId: project.id,
        worktreeId: 'wt-main',
        createdAt: '2026-07-14T00:00:00.000Z',
        updatedAt: '2026-07-14T00:00:00.000Z',
      });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' })]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      inspectorTab: 'files',
      translateError: (key) =>
        key === 'mutationUnknown' ? 'Result unknown. Refresh and verify manually.' : `err:${key}`,
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });
    expect(result.current.unknownMutationLock?.clientOperationId).toBe('op-stable');
    expect(fakeWorktreesApi.commit).toHaveBeenCalledTimes(1);

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    // second click 只对账，不再 mint 新 id / 二次 commit。
    expect(fakeWorktreesApi.commit).toHaveBeenCalledTimes(1);
    expect(fakeWorktreesApi.getMutationOperation).toHaveBeenCalledTimes(2);
    expect(fakeWorktreesApi.getMutationOperation).toHaveBeenLastCalledWith('op-stable');
    expect(result.current.unknownMutationLock).toBeNull();
    expect(result.current.worktreeError).toBeNull();
  });

  test('ledger terminal failed clears unknown lock and allows new id', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.commit.mockResolvedValueOnce({
      kind: 'unknown',
      clientOperationId: 'op-fail',
    });
    fakeWorktreesApi.getMutationOperation.mockResolvedValueOnce({
      clientOperationId: 'op-fail',
      kind: 'commit',
      payloadHash: 'h',
      intent: {
        kind: 'commit',
        projectId: project.id,
        worktreeId: 'wt-main',
        beforeHead: 'a',
        expectedTree: 't',
      },
      state: 'failed',
      outcome: null,
      errorMessage: 'boom',
      projectId: project.id,
      worktreeId: 'wt-main',
      createdAt: '2026-07-14T00:00:00.000Z',
      updatedAt: '2026-07-14T00:00:00.000Z',
    });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' })]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    expect(result.current.unknownMutationLock).toBeNull();
    expect(result.current.worktreeError).toBeTruthy();
  });

  test('after commit unknown, sibling push/merge/remove are blocked and do not mint Fresh ids', async () => {
    const project = buildLocalProject();
    const featWt = buildWorktree({
      id: 'wt-feat',
      isMain: false,
      name: 'feat',
      branch: 'feature/feat',
      status: {
        branch: 'feature/feat',
        changed: 0,
        ahead: 1,
        behind: 0,
        conflicts: 0,
        clean: true,
        canPush: true,
      },
    });
    fakeWorktreesApi.commit.mockResolvedValueOnce({
      kind: 'unknown',
      clientOperationId: 'op-commit-lock',
      transportClass: 'timeout',
    });
    fakeWorktreesApi.getMutationOperation.mockResolvedValue({
      clientOperationId: 'op-commit-lock',
      kind: 'commit',
      payloadHash: 'h',
      intent: {
        kind: 'commit',
        projectId: project.id,
        worktreeId: 'wt-feat',
        beforeHead: 'a',
        expectedTree: 't',
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: project.id,
      worktreeId: 'wt-feat',
      createdAt: '2026-07-14T00:00:00.000Z',
      updatedAt: '2026-07-14T00:00:00.000Z',
    });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' }), featWt]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-feat',
      inspectorTab: 'files',
      confirmAction: () => true,
      translateError: (key) =>
        key === 'mutationUnknown' ? '结果未知，请刷新后人工核对' : `err:${key}`,
    });

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    expect(result.current.unknownMutationLock).toEqual({
      kind: 'commit',
      projectId: project.id,
      worktreeId: 'wt-feat',
      clientOperationId: 'op-commit-lock',
    });

    await act(async () => {
      await result.current.handlePushWorktree();
      await flushMicrotasks();
    });
    await act(async () => {
      await result.current.handleMergeWorktree();
      await flushMicrotasks();
    });
    await act(async () => {
      await result.current.handleRemoveWorktree();
      await flushMicrotasks();
    });

    expect(fakeWorktreesApi.push).not.toHaveBeenCalled();
    expect(fakeWorktreesApi.merge).not.toHaveBeenCalled();
    expect(fakeWorktreesApi.remove).not.toHaveBeenCalled();
    expect(result.current.unknownMutationLock).toEqual({
      kind: 'commit',
      projectId: project.id,
      worktreeId: 'wt-feat',
      clientOperationId: 'op-commit-lock',
    });

    // same-kind second click still only reconciles with stable id
    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });
    expect(fakeWorktreesApi.commit).toHaveBeenCalledTimes(1);
    expect(fakeWorktreesApi.getMutationOperation).toHaveBeenLastCalledWith('op-commit-lock');
  });

  test('sibling success cannot clear an unrelated unknownMutationLock', async () => {
    const project = buildLocalProject();
    const featWt = buildWorktree({
      id: 'wt-feat',
      isMain: false,
      name: 'feat',
      branch: 'feature/feat',
      status: {
        branch: 'feature/feat',
        changed: 0,
        ahead: 1,
        behind: 0,
        conflicts: 0,
        clean: true,
        canPush: true,
      },
    });
    fakeWorktreesApi.commit.mockResolvedValueOnce({
      kind: 'unknown',
      clientOperationId: 'op-commit-keep',
      transportClass: 'timeout',
    });
    fakeWorktreesApi.getMutationOperation.mockResolvedValue({
      clientOperationId: 'op-commit-keep',
      kind: 'commit',
      payloadHash: 'h',
      intent: {
        kind: 'commit',
        projectId: project.id,
        worktreeId: 'wt-feat',
        beforeHead: 'a',
        expectedTree: 't',
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: project.id,
      worktreeId: 'wt-feat',
      createdAt: '2026-07-14T00:00:00.000Z',
      updatedAt: '2026-07-14T00:00:00.000Z',
    });
    fakeWorktreesApi.push.mockResolvedValueOnce(succeededEnvelope(featWt, 'op-push-sibling'));
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' }), featWt]);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-feat',
      inspectorTab: 'files',
      translateError: (key) =>
        key === 'mutationUnknown' ? '结果未知' : `err:${key}`,
    });

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });
    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });
    expect(result.current.unknownMutationLock?.clientOperationId).toBe('op-commit-keep');

    // Even if push handler is invoked, it must not clear the commit lock.
    await act(async () => {
      await result.current.handlePushWorktree();
      await flushMicrotasks();
    });
    expect(fakeWorktreesApi.push).not.toHaveBeenCalled();
    expect(result.current.unknownMutationLock).toEqual({
      kind: 'commit',
      projectId: project.id,
      worktreeId: 'wt-feat',
      clientOperationId: 'op-commit-keep',
    });
  });
});

/* ---------------------------------------------------------------------------
 * remove
 * ------------------------------------------------------------------------- */

describe('useWorkbenchWorktreeGitController — remove', () => {
  test('handleRemoveWorktree asks confirm, removes, switches active off removed worktree, reloads list; uses bridge, never terminal state directly', async () => {
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', isMain: true });
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false, name: 'feat', branch: 'feature/feat' });
    fakeWorktreesApi.list.mockResolvedValue([mainWt, featWt]);
    fakeWorktreesApi.remove.mockResolvedValueOnce(
      succeededEnvelope({ ok: true, worktreeId: 'wt-feat' }),
    );

    const confirmAction = vi.fn(() => true);
    const bridge = buildBridgeFakes();
    const setActive = vi.fn();

    const { result, activeWorktreeState } = renderController(
      {
        activeProjectId: project.id,
        activeWorktreeId: 'wt-feat',
        confirmAction,
        setActiveWorktreeId: setActive,
      },
      bridge,
    );

    // seed worktrees list first.
    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleRemoveWorktree();
      await flushMicrotasks();
    });

    expect(confirmAction).toHaveBeenCalled();
    expect(fakeWorktreesApi.remove).toHaveBeenCalledWith(
      'wt-feat',
      false,
      expect.any(String),
    );
    // active worktree switched off the removed one (back to wt-main) via page setter。
    expect(setActive).toHaveBeenCalledWith('wt-main');
    expect(activeWorktreeState.value).toBe('wt-main');
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('handleRemoveWorktree aborts when confirm denied', async () => {
    const project = buildLocalProject();
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' }), featWt]);
    const confirmAction = vi.fn(() => false);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-feat',
      confirmAction,
    });

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleRemoveWorktree();
      await flushMicrotasks();
    });

    expect(fakeWorktreesApi.remove).not.toHaveBeenCalled();
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('handleRemoveWorktree is no-op on main worktree', async () => {
    const { result } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: 'wt-main',
    });

    await act(async () => {
      await result.current.handleRemoveWorktree();
      await flushMicrotasks();
    });
    expect(fakeWorktreesApi.remove).not.toHaveBeenCalled();
  });

  test('handleRemoveWorktree surfaces error and marks failure on throw', async () => {
    const project = buildLocalProject();
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' }), featWt]);
    fakeWorktreesApi.remove.mockRejectedValueOnce(new Error('remove failed'));
    const markFailure = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-feat',
      markRequestFailure: markFailure,
    });

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleRemoveWorktree();
      await flushMicrotasks();
    });

    expect(result.current.worktreeError).toContain('remove failed');
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
  });
});

/* ---------------------------------------------------------------------------
 * merge
 * ------------------------------------------------------------------------- */

describe('useWorkbenchWorktreeGitController — merge', () => {
  test('handleMergeWorktree confirms, sets initial running stage, calls merge, reloads worktrees + sessions, clears buffers, refreshes git history', async () => {
    const project = buildLocalProject();
    const mainWt = buildWorktree({ id: 'wt-main', isMain: true });
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false, name: 'feat' });
    const mergeResult: WorkbenchMergeResult = {
      ok: true,
      worktreeId: 'wt-feat',
      stages: [
        { id: 'checkSource', status: 'completed', message: 'ok' },
        { id: 'cleanup', status: 'completed', message: 'done' },
      ],
    };
    fakeWorktreesApi.merge.mockResolvedValueOnce(succeededEnvelope(mergeResult));
    fakeGitApi.listCommits.mockResolvedValueOnce([buildCommit()]);
    const confirmAction = vi.fn(() => true);
    const refreshStats = vi.fn();
    const bridge = buildBridgeFakes();

    const { result } = renderController(
      {
        activeProjectId: project.id,
        activeWorktreeId: 'wt-feat',
        inspectorTab: 'history',
        confirmAction,
        refreshProjectSessionStats: refreshStats,
      },
      bridge,
    );

    // seed + post-merge/history reconcile all need a stable list containing active feature.
    fakeWorktreesApi.list.mockResolvedValue([mainWt, featWt]);
    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleMergeWorktree();
      await flushMicrotasks();
    });

    expect(confirmAction).toHaveBeenCalled();
    expect(fakeWorktreesApi.merge).toHaveBeenCalledWith('wt-feat', expect.any(String));
    // merge -> loadWorktrees -> loadSessions (bridge) -> clearBuffersForWorktree (bridge) -> loadGitHistory.
    expect(bridge.loadSessions).toHaveBeenCalledWith(project.id);
    expect(bridge.clearBuffersForWorktree).toHaveBeenCalledWith('wt-feat');
    expect(fakeGitApi.listCommits).toHaveBeenCalledWith(project.id, 'wt-feat', 30);
    expect(refreshStats).toHaveBeenCalledWith(project.id);
    expect(result.current.worktreeBusy).toBeNull();
    // after successful merge with cleanup completed + no failed/running, auto-dismiss scheduled.
  });

  test('handleMergeWorktree aborts when confirm denied', async () => {
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' }), featWt]);
    const confirmAction = vi.fn(() => false);

    const { result } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: 'wt-feat',
      confirmAction,
    });

    await act(async () => {
      await result.current.loadWorktrees('project-1');
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleMergeWorktree();
      await flushMicrotasks();
    });

    expect(fakeWorktreesApi.merge).not.toHaveBeenCalled();
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('handleMergeWorktree is no-op on main worktree', async () => {
    const { result } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: 'wt-main',
    });

    await act(async () => {
      await result.current.handleMergeWorktree();
      await flushMicrotasks();
    });
    expect(fakeWorktreesApi.merge).not.toHaveBeenCalled();
  });

  test('handleMergeWorktree marks running stage as failed on error and still reloads worktrees + sessions', async () => {
    const project = buildLocalProject();
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' }), featWt]);
    fakeWorktreesApi.merge.mockRejectedValueOnce(new Error('merge boom'));
    const markFailure = vi.fn();
    const bridge = buildBridgeFakes();

    const { result } = renderController(
      {
        activeProjectId: project.id,
        activeWorktreeId: 'wt-feat',
        inspectorTab: 'files',
        markRequestFailure: markFailure,
      },
      bridge,
    );

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleMergeWorktree();
      await flushMicrotasks();
    });

    expect(result.current.worktreeError).toContain('merge boom');
    expect(bridge.loadSessions).toHaveBeenCalledWith(project.id);
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
    // at least one stage should be marked failed.
    expect(result.current.mergeStages.some((s) => s.status === 'failed')).toBe(true);
    expect(result.current.worktreeBusy).toBeNull();
  });

  test('successful merge auto-dismisses merge stage panel after timeout', async () => {
    const project = buildLocalProject();
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' }), featWt]);
    fakeWorktreesApi.merge.mockResolvedValueOnce(
      succeededEnvelope({
        ok: true,
        worktreeId: 'wt-feat',
        stages: [{ id: 'cleanup', status: 'completed', message: 'done' }],
      } as WorkbenchMergeResult),
    );
    const bridge = buildBridgeFakes();

    const { result } = renderController(
      {
        activeProjectId: project.id,
        activeWorktreeId: 'wt-feat',
        inspectorTab: 'files',
      },
      bridge,
    );

    await act(async () => {
      await result.current.loadWorktrees(project.id);
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleMergeWorktree();
      await flushMicrotasks();
    });

    expect(result.current.mergeStages.length).toBeGreaterThan(0);

    await act(async () => {
      vi.advanceTimersByTime(3000);
      await flushMicrotasks();
    });

    expect(result.current.mergeStages).toEqual([]);
    expect(result.current.mergeProgressWorktreeId).toBeNull();
  });

  test('clearMergeStagePanel immediately clears stages and tracked worktree', async () => {
    const featWt = buildWorktree({ id: 'wt-feat', isMain: false });
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ id: 'wt-main' }), featWt]);

    const { result } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: 'wt-feat',
    });

    await act(async () => {
      await result.current.loadWorktrees('project-1');
      await flushMicrotasks();
    });

    // trigger merge to populate stages, then clear.
    fakeWorktreesApi.merge.mockResolvedValueOnce(
      succeededEnvelope({
        ok: true,
        worktreeId: 'wt-feat',
        stages: [{ id: 'cleanup', status: 'failed', message: 'nope' }],
      } as WorkbenchMergeResult),
    );
    await act(async () => {
      await result.current.handleMergeWorktree();
      await flushMicrotasks();
    });
    expect(result.current.mergeStages.length).toBeGreaterThan(0);

    act(() => {
      result.current.clearMergeStagePanel();
    });
    expect(result.current.mergeStages).toEqual([]);
    expect(result.current.mergeProgressWorktreeId).toBeNull();
  });
});

/* ---------------------------------------------------------------------------
 * git history refresh
 * ------------------------------------------------------------------------- */

describe('useWorkbenchWorktreeGitController — git history refresh', () => {
  test('loadGitHistory stores commits, marks success; clears on missing project', async () => {
    const project = buildLocalProject();
    const commits = [buildCommit({ hash: 'a' }), buildCommit({ hash: 'b' })];
    const main = buildWorktree();
    fakeWorktreesApi.list.mockResolvedValueOnce([main]);
    fakeGitApi.listCommits.mockResolvedValueOnce(commits);
    const markSuccess = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      markRequestSuccess: markSuccess,
    });

    await act(async () => {
      await result.current.loadGitHistory();
      await flushMicrotasks();
    });

    expect(fakeWorktreesApi.list).toHaveBeenCalledWith(project.id);
    expect(fakeGitApi.listCommits).toHaveBeenCalledWith(project.id, 'wt-main', 30);
    expect(result.current.gitCommits).toEqual(commits);
    expect(result.current.gitHistoryLoading).toBe(false);
    expect(result.current.gitHistoryError).toBeNull();
    expect(markSuccess).toHaveBeenCalledWith(project.id);
  });

  test('loadGitHistory drops stale response after project switches', async () => {
    const projectA = buildLocalProject({ id: 'p-a' });
    const commits = [buildCommit({ hash: 'a' })];
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree({ projectId: 'p-a' })]);
    // 用可控 deferred 让响应在项目切换后才 resolve。
    let resolveList: (value: WorkbenchGitCommit[]) => void = () => undefined;
    fakeGitApi.listCommits.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveList = resolve;
        }),
    );
    // 模拟项目域 controller 的 isCurrentProject：读取可变 currentProjectId（与 ref 行为一致），
    // 这样在 rerender 前后闭包共享同一份状态，正确反映 stale guard。
    const current = { projectId: projectA.id };
    const isCurrentProject = (id: string): boolean => id === current.projectId;

    const { result, rerender } = renderController({
      activeProjectId: projectA.id,
      activeWorktreeId: 'wt-main',
      isCurrentProject,
    });

    // 发起 loadGitHistory（请求 p-a），尚未 resolve。
    let loadPromise: Promise<void> | undefined;
    act(() => {
      loadPromise = result.current.loadGitHistory();
    });
    await act(async () => {
      await flushMicrotasks(2);
    });

    // 项目切到 p-b；isCurrentProject('p-a') 现在返回 false。
    current.projectId = 'p-b';
    rerender(
      baseControllerProps({
        activeProjectId: 'p-b',
        activeWorktreeId: 'wt-main',
        isCurrentProject,
      }),
    );
    await act(async () => {
      await flushMicrotasks(2);
    });

    // 现在 resolve p-a 的响应；stale guard 应丢弃。
    await act(async () => {
      resolveList(commits);
      await loadPromise;
      await flushMicrotasks();
    });

    expect(result.current.gitCommits).toEqual([]);
  });

  test('slow initial loadGitHistory does not overwrite later refresh for same worktree', async () => {
    const project = buildLocalProject();
    const oldCommits = [buildCommit({ hash: 'old' })];
    const newCommits = [buildCommit({ hash: 'new1' }), buildCommit({ hash: 'new2' })];
    fakeWorktreesApi.list.mockResolvedValue([buildWorktree()]);

    let resolveInitial: (value: typeof oldCommits) => void = () => undefined;
    fakeGitApi.listCommits
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveInitial = resolve as (value: typeof oldCommits) => void;
          }),
      )
      .mockResolvedValueOnce(newCommits);

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
    });

    let firstLoad: Promise<void> | undefined;
    act(() => {
      firstLoad = result.current.loadGitHistory();
    });
    await act(async () => {
      await flushMicrotasks(2);
    });

    await act(async () => {
      await result.current.loadGitHistory();
      await flushMicrotasks();
    });
    expect(result.current.gitCommits).toEqual(newCommits);
    expect(result.current.gitHistoryLoading).toBe(false);

    await act(async () => {
      resolveInitial(oldCommits);
      await firstLoad;
      await flushMicrotasks();
    });

    expect(result.current.gitCommits).toEqual(newCommits);
    expect(result.current.gitHistoryLoading).toBe(false);
  });

  test('loadGitHistory surfaces error and clears commits on throw', async () => {
    const project = buildLocalProject();
    fakeWorktreesApi.list.mockResolvedValueOnce([buildWorktree()]);
    fakeGitApi.listCommits.mockRejectedValueOnce(new Error('git down'));
    const markFailure = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      markRequestFailure: markFailure,
    });

    await act(async () => {
      await result.current.loadGitHistory();
      await flushMicrotasks();
    });

    expect(result.current.gitCommits).toEqual([]);
    expect(result.current.gitHistoryError).toContain('git down');
    expect(result.current.gitHistoryLoading).toBe(false);
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
  });

  test('loadGitHistory reconciles worktrees and loads history for fallback active after prune', async () => {
    const project = buildLocalProject();
    const main = buildWorktree({ id: 'wt-main', name: 'main', isMain: true });
    // Active feature worktree was cleaned externally; list only returns main after sync_git_worktrees.
    fakeWorktreesApi.list.mockResolvedValueOnce([main]);
    const mainCommits = [buildCommit({ hash: 'main-head' })];
    fakeGitApi.listCommits.mockResolvedValueOnce(mainCommits);
    const setActive = vi.fn();

    const { result, activeWorktreeState } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-feature',
      setActiveWorktreeId: setActive,
    });

    await act(async () => {
      await result.current.loadGitHistory();
      await flushMicrotasks();
    });

    expect(fakeWorktreesApi.list).toHaveBeenCalledWith(project.id);
    expect(result.current.worktrees.map((wt) => wt.id)).toEqual(['wt-main']);
    expect(setActive).toHaveBeenCalledWith('wt-main');
    expect(activeWorktreeState.value).toBe('wt-main');
    expect(fakeGitApi.listCommits).toHaveBeenCalledWith(project.id, 'wt-main', 30);
    expect(result.current.gitCommits).toEqual(mainCommits);
    expect(result.current.gitHistoryError).toBeNull();
  });

  test('loadGitHistory still refreshes commits when worktree reconcile fails', async () => {
    const project = buildLocalProject();
    const commits = [buildCommit({ hash: 'kept' })];
    fakeWorktreesApi.list.mockRejectedValueOnce(new Error('worktree list down'));
    fakeGitApi.listCommits.mockResolvedValueOnce(commits);
    const markFailure = vi.fn();
    const markSuccess = vi.fn();

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
      markRequestFailure: markFailure,
      markRequestSuccess: markSuccess,
    });

    await act(async () => {
      await result.current.loadGitHistory();
      await flushMicrotasks();
    });

    expect(result.current.worktreeError).toContain('worktree list down');
    expect(fakeGitApi.listCommits).toHaveBeenCalledWith(project.id, 'wt-main', 30);
    expect(result.current.gitCommits).toEqual(commits);
    expect(result.current.gitHistoryError).toBeNull();
    expect(markFailure).toHaveBeenCalledWith(project.id, expect.any(Error));
    expect(markSuccess).toHaveBeenCalledWith(project.id);
  });
});

/* ---------------------------------------------------------------------------
 * merge-progress event filtering
 * ------------------------------------------------------------------------- */

describe('useWorkbenchWorktreeGitController — merge-progress event filtering', () => {
  test('merge-progress for current project is tracked; different project is ignored', async () => {
    const { result, bridge: _bridge } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: 'wt-main',
      canListenToTauriEvents: () => true,
    });
    void _bridge;

    await act(async () => {
      await flushMicrotasks();
    });

    // event for OTHER project should be ignored.
    emitEvent('workbench:merge-progress', {
      projectId: 'p-other',
      worktreeId: 'wt-other',
      stage: { id: 'checkSource', status: 'running', message: 'other' },
    });
    await act(async () => {
      await flushMicrotasks();
    });
    expect(result.current.mergeStages).toEqual([]);
    expect(result.current.mergeProgressWorktreeId).toBeNull();

    // event for current project should be tracked.
    emitEvent('workbench:merge-progress', {
      projectId: 'project-1',
      worktreeId: 'wt-feat',
      stage: { id: 'checkSource', status: 'running', message: 'checking' },
    });
    await act(async () => {
      await flushMicrotasks();
    });
    expect(result.current.mergeProgressWorktreeId).toBe('wt-feat');
    expect(result.current.mergeStages.some((s) => s.id === 'checkSource' && s.status === 'running')).toBe(true);
  });

  test('merge-progress for a different worktree than currently tracked is ignored once tracking started', async () => {
    const { result } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: 'wt-main',
    });

    await act(async () => {
      await flushMicrotasks();
    });

    emitEvent('workbench:merge-progress', {
      projectId: 'project-1',
      worktreeId: 'wt-feat',
      stage: { id: 'checkSource', status: 'running', message: 'a' },
    });
    await act(async () => {
      await flushMicrotasks();
    });
    expect(result.current.mergeProgressWorktreeId).toBe('wt-feat');

    emitEvent('workbench:merge-progress', {
      projectId: 'project-1',
      worktreeId: 'wt-other',
      stage: { id: 'mergeMain', status: 'running', message: 'b' },
    });
    await act(async () => {
      await flushMicrotasks();
    });
    // still tracking wt-feat; wt-other ignored — mergeMain remains pending (not running).
    expect(result.current.mergeProgressWorktreeId).toBe('wt-feat');
    const mergeMainStage = result.current.mergeStages.find((s) => s.id === 'mergeMain');
    expect(mergeMainStage?.status).toBe('pending');
    expect(mergeMainStage?.message).toBe('');
  });

  test('does not register listener when canListenToTauriEvents returns false', async () => {
    renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: 'wt-main',
      canListenToTauriEvents: () => false,
    });

    await act(async () => {
      await flushMicrotasks();
    });

    // no listener registered.
    expect(eventListeners.has('workbench:merge-progress')).toBe(false);
  });
});

/* ---------------------------------------------------------------------------
 * hookRepair — failedHook envelope + context switch + dismiss
 * ------------------------------------------------------------------------- */

function buildHookFailure(overrides: Partial<WorkbenchHookFailure> = {}): WorkbenchHookFailure {
  return {
    stage: 'preCommit',
    stdout: 'lint failed',
    stderr: 'error: trailing whitespace',
    exitCode: 1,
    ...overrides,
  };
}

function failedHookEnvelope(hookFailure: WorkbenchHookFailure, clientOperationId = 'op-hook'): {
  kind: 'failedHook';
  clientOperationId: string;
  hookFailure: WorkbenchHookFailure;
} {
  return { kind: 'failedHook', clientOperationId, hookFailure };
}

describe('useWorkbenchWorktreeGitController — hookRepair', () => {
  test('failedHook commit envelope sets hookRepair; switching activeProjectId clears it', async () => {
    const project = buildLocalProject();
    const hookFailure = buildHookFailure();
    fakeWorktreesApi.commit.mockResolvedValueOnce(failedHookEnvelope(hookFailure));

    const { result, rerender } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    // failedHook 后 controller 持有 hookRepair；worktreeError 不被覆盖。
    expect(result.current.hookRepair).not.toBeNull();
    expect(result.current.hookRepair?.kind).toBe('commit');
    expect(result.current.hookRepair?.hookFailure).toEqual(hookFailure);
    expect(result.current.hookRepair?.clientOperationId).toBe('op-hook');
    expect(result.current.worktreeError).toBeNull();

    // 切到其他工作台项目：hookRepair 必须清空，避免面板粘到新上下文。
    rerender(
      baseControllerProps({
        activeProjectId: 'project-2',
        activeWorktreeId: 'wt-main',
      }),
    );
    await act(async () => {
      await flushMicrotasks();
    });

    expect(result.current.hookRepair).toBeNull();
  });

  test('failedHook commit envelope sets hookRepair; switching activeWorktreeId clears it', async () => {
    const project = buildLocalProject();
    const hookFailure = buildHookFailure();
    fakeWorktreesApi.commit.mockResolvedValueOnce(failedHookEnvelope(hookFailure));

    const { result, rerender } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    expect(result.current.hookRepair).not.toBeNull();

    // 同 project 内切到非主 worktree：stale hookRepair 必须清空。
    rerender(
      baseControllerProps({
        activeProjectId: project.id,
        activeWorktreeId: 'wt-feat',
      }),
    );
    await act(async () => {
      await flushMicrotasks();
    });

    expect(result.current.hookRepair).toBeNull();
  });

  test('handleDismissHookFailure clears hookRepair without calling any API', async () => {
    const project = buildLocalProject();
    const hookFailure = buildHookFailure();
    fakeWorktreesApi.commit.mockResolvedValueOnce(failedHookEnvelope(hookFailure));

    const { result } = renderController({
      activeProjectId: project.id,
      activeWorktreeId: 'wt-main',
    });

    await act(async () => {
      await result.current.handleCommitWorktree();
      await flushMicrotasks();
    });

    expect(result.current.hookRepair).not.toBeNull();

    const callsBefore = {
      commit: fakeWorktreesApi.commit.mock.calls.length,
      push: fakeWorktreesApi.push.mock.calls.length,
      list: fakeWorktreesApi.list.mock.calls.length,
      repair: fakeWorktreesApi.repairHookFailure.mock.calls.length,
    };

    await act(async () => {
      await result.current.handleDismissHookFailure();
      await flushMicrotasks();
    });

    expect(result.current.hookRepair).toBeNull();
    // dismiss 是纯本地动作，不得触发任何 IPC。
    expect(fakeWorktreesApi.commit.mock.calls.length).toBe(callsBefore.commit);
    expect(fakeWorktreesApi.push.mock.calls.length).toBe(callsBefore.push);
    expect(fakeWorktreesApi.list.mock.calls.length).toBe(callsBefore.list);
    expect(fakeWorktreesApi.repairHookFailure.mock.calls.length).toBe(callsBefore.repair);
  });

  test('handleDismissHookFailure is a safe no-op when hookRepair is null', async () => {
    const { result } = renderController({
      activeProjectId: 'project-1',
      activeWorktreeId: 'wt-main',
    });

    expect(result.current.hookRepair).toBeNull();

    await act(async () => {
      await result.current.handleDismissHookFailure();
      await flushMicrotasks();
    });

    expect(result.current.hookRepair).toBeNull();
  });
});
