// @vitest-environment jsdom
/**
 * useWorkspaceSafeRestore 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   「打开项目默认进终端」的产品诉求由 useWorkspaceSafeRestore 的 bridge 强制行为实现。
 *   必须独立可测,确保初始 restore 路径无视 plan.workspaceView 写 terminal,
 *   命名 snapshot apply 路径仍尊重快照的 workspaceView,apply 失败回滚 previous
 *   不被强制逻辑污染。
 *
 * Code Logic（这个测试做什么）:
 *   - 用 vi.mock 接管 @/api/workbench.layout.preflight / apply / save / get;
 *   - 通过 renderHook 把 hook 挂在 React 树中,提供完整 params;
 *   - mock setWorkspaceView / setInspectorTab / selectProjectFromDeepLink / focusSession
 *     等回调,断言在 restore flow 中实际接收到的值;
 *   - 用 fake timers 跳过 suppress 窗口的 50ms setTimeout;
 *   - 覆盖四组用例:初始 restore force terminal (files)、初始 restore force terminal (browser)、
 *     命名 snapshot 保留 view、apply 异常回滚 previous。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

import { useWorkspaceSafeRestore } from './useWorkspaceSafeRestore';
import type { UseWorkspaceSafeRestoreResult } from './useWorkspaceSafeRestore';
import type { WorkbenchFileWorkspaceView } from './workbenchFiles';
import type { WorkbenchInspectorTab } from './WorkbenchInspector';
import type { WorkspaceRestorePlan } from './workspaceRestore';

/* ---------------------------------------------------------------------------
 * vi.mock — workbench layout API
 *
 * Business Logic: hook 单元测试不应触发真实 Tauri invoke;用 fake 记录所有 layout
 * 调用,并允许测试动态设置返回值或抛出错误。
 * ------------------------------------------------------------------------- */

const layoutApi = vi.hoisted(() => ({
  preflight: vi.fn(),
  apply: vi.fn(),
  save: vi.fn(),
  get: vi.fn(),
  listNamed: vi.fn(),
  deleteNamed: vi.fn(),
}));

const browserApi = vi.hoisted(() => ({
  createPreview: vi.fn(),
}));

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    layout: layoutApi,
    browser: browserApi,
  },
}));

/* ---------------------------------------------------------------------------
 * Plan factory — build a complete plan with overrides for view/tab/actions.
 *
 * `workspaceView` 字段是 plan 内部 layout 枚举 ('terminal'|'files'|'browser'|'automation')，
 * 用于驱动 bridge.setWorkspaceView。InspectorTab 是完整布局枚举
 * ('files'|'git'|'history'|'notes'|'automation')；hook 通过 fromLayoutInspectorTab 折叠到 UI 枚举。
 * ------------------------------------------------------------------------- */

function buildPlan(overrides: {
  workspaceView?: WorkspaceRestorePlan['workspaceView'];
  inspectorTab?: WorkspaceRestorePlan['inspectorTab'];
  actions?: WorkspaceRestorePlan['actions'];
} = {}): WorkspaceRestorePlan {
  const workspaceView = overrides.workspaceView ?? 'files';
  const inspectorTab = overrides.inspectorTab ?? 'history';
  const actions = overrides.actions ?? [
    { target: 'project' as const, resourceId: 'p1', outcome: 'select' as const },
    { target: 'worktree' as const, resourceId: 'w1', outcome: 'select' as const },
    { target: 'session' as const, resourceId: 's1', outcome: 'select' as const },
    { target: 'workspaceView' as const, resourceId: workspaceView, outcome: 'select' as const },
    { target: 'inspectorTab' as const, resourceId: inspectorTab, outcome: 'select' as const },
  ];
  return {
    restoreId: 'r1',
    layoutId: 'L1',
    layoutRevision: 1,
    status: 'complete',
    resolvedProjectId: 'p1',
    resolvedWorktreeId: 'w1',
    resolvedSessionId: 's1',
    workspaceView,
    inspectorTab,
    browserTargetUrl: null,
    actions,
  };
}

/* ---------------------------------------------------------------------------
 * Harness — wraps renderHook with stable callbacks and captures calls.
 * ------------------------------------------------------------------------- */

interface HookParams {
  projectsLoading: boolean;
  projectsLength: number;
  activeProjectId: string | null;
  activeWorktreeId: string | null;
  activeSessionId: string | null;
  workspaceView: WorkbenchFileWorkspaceView;
  inspectorTab: WorkbenchInspectorTab;
  browserTargetUrl: string | null;
  dirtyEditor: boolean;
  layoutSlotKey?: string;
  urlProjectId?: string | null;
  browserEnabled?: boolean;
}

interface HarnessReturn {
  hook: { readonly current: UseWorkspaceSafeRestoreResult };
  rerender: () => void;
  calls: {
    setWorkspaceView: WorkbenchFileWorkspaceView[];
    setInspectorTab: WorkbenchInspectorTab[];
    setActiveWorktreeId: (string | null)[];
    setBrowserTargetUrl: (string | null)[];
    selectProjectFromDeepLink: string[];
    focusSession: string[];
  };
  refs: {
    activeProjectIdRef: React.MutableRefObject<string | null>;
    activeWorktreeIdRef: React.MutableRefObject<string | null>;
  };
}

function makeHarness(params: HookParams): HarnessReturn {
  const calls = {
    setWorkspaceView: [] as WorkbenchFileWorkspaceView[],
    setInspectorTab: [] as WorkbenchInspectorTab[],
    setActiveWorktreeId: [] as (string | null)[],
    setBrowserTargetUrl: [] as (string | null)[],
    selectProjectFromDeepLink: [] as string[],
    focusSession: [] as string[],
  };

  const selectProjectFromDeepLink = vi.fn(async (id: string) => {
    calls.selectProjectFromDeepLink.push(id);
    return true;
  });
  const setActiveWorktreeId = vi.fn((id: string | null) => {
    calls.setActiveWorktreeId.push(id);
  });
  const focusSession = vi.fn(async (id: string) => {
    calls.focusSession.push(id);
    return true;
  });
  const setWorkspaceView = vi.fn((view: WorkbenchFileWorkspaceView) => {
    calls.setWorkspaceView.push(view);
  });
  const setInspectorTab = vi.fn((tab: WorkbenchInspectorTab) => {
    calls.setInspectorTab.push(tab);
  });
  const setBrowserTargetUrl = vi.fn((url: string | null) => {
    calls.setBrowserTargetUrl.push(url);
  });

  const refs = {
    activeProjectIdRef: { current: params.activeProjectId } as React.MutableRefObject<string | null>,
    activeWorktreeIdRef: {
      current: params.activeWorktreeId,
    } as React.MutableRefObject<string | null>,
  };
  // Keep refs in sync with current params so post-apply code reads latest values.
  refs.activeProjectIdRef.current = params.activeProjectId;
  refs.activeWorktreeIdRef.current = params.activeWorktreeId;

  const {
    result: hook,
    rerender,
  } = renderHook<UseWorkspaceSafeRestoreResult, unknown>(() =>
    useWorkspaceSafeRestore({
      projectsLoading: params.projectsLoading,
      projectsLength: params.projectsLength,
      activeProjectId: params.activeProjectId,
      activeWorktreeId: params.activeWorktreeId,
      activeSessionId: params.activeSessionId,
      workspaceView: params.workspaceView,
      inspectorTab: params.inspectorTab,
      browserTargetUrl: params.browserTargetUrl,
      dirtyEditor: params.dirtyEditor,
      activeProjectIdRef: refs.activeProjectIdRef,
      activeWorktreeIdRef: refs.activeWorktreeIdRef,
      selectProjectFromDeepLink,
      setActiveWorktreeId,
      focusSession,
      setWorkspaceView,
      setInspectorTab,
      setBrowserTargetUrl,
      layoutSlotKey: params.layoutSlotKey,
      urlProjectId: params.urlProjectId,
      browserEnabled: params.browserEnabled ?? true,
    }),
  );

  return { hook, rerender, calls, refs };
}

/* ---------------------------------------------------------------------------
 * Test suite
 * ------------------------------------------------------------------------- */

describe('useWorkspaceSafeRestore — initial restore force terminal', () => {
  beforeEach(() => {
    layoutApi.preflight.mockReset();
    layoutApi.apply.mockReset();
    layoutApi.save.mockReset();
    layoutApi.get.mockReset();
    layoutApi.listNamed.mockReset();
    layoutApi.deleteNamed.mockReset();
    browserApi.createPreview.mockReset();

    layoutApi.apply.mockResolvedValue({
      restoreId: 'r1',
      status: 'complete',
      restoredCount: 0,
      skippedCount: 0,
      actions: [],
    });
    layoutApi.get.mockResolvedValue(null);
    // autosave coordinator runs whenever selection changes; default it to a
    // valid WorkspaceLayout so the saveWithCas promise chain doesn't reject.
    layoutApi.save.mockImplementation(
      async (draft: { projectId: string; slotKey: string; kind: string; name?: string }) => ({
        schemaVersion: 1,
        id: 'autosave-1',
        revision: 1,
        slotKey: draft.slotKey,
        kind: draft.kind,
        name: draft.name ?? null,
        projectId: draft.projectId,
        activeWorktreeId: null,
        activeSessionId: null,
        workspaceView: 'terminal' as const,
        inspectorTab: 'files' as const,
        browserTargetUrl: null,
        createdAt: '2026-08-01T00:00:00.000Z',
        updatedAt: '2026-08-01T00:00:00.000Z',
      }),
    );
  });

  afterEach(() => {
    cleanup();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  test('initial restore writes workspaceView=terminal even when plan says files', async () => {
    vi.useFakeTimers();
    layoutApi.preflight.mockResolvedValue(
      buildPlan({ workspaceView: 'files', inspectorTab: 'history' }),
    );

    const { calls } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: null,
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await vi.runAllTimersAsync();
    });

    expect(layoutApi.preflight).toHaveBeenCalledTimes(1);
    expect(calls.setWorkspaceView).toContain('terminal');
    // bridge must NOT write 'files' even though plan says so.
    expect(calls.setWorkspaceView).not.toContain('files');
    // inspectorTab should be preserved from plan (per product decision).
    expect(calls.setInspectorTab).toContain('history');
  });

  test('initial restore maps layout inspectorTab notes to UI notes', async () => {
    vi.useFakeTimers();
    layoutApi.preflight.mockResolvedValue(
      buildPlan({ workspaceView: 'terminal', inspectorTab: 'notes' }),
    );

    const { calls } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: null,
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await vi.runAllTimersAsync();
    });

    expect(calls.setInspectorTab).toContain('notes');
  });

  test('initial restore maps layout inspectorTab git to UI history', async () => {
    vi.useFakeTimers();
    layoutApi.preflight.mockResolvedValue(
      buildPlan({ workspaceView: 'terminal', inspectorTab: 'git' }),
    );

    const { calls } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: null,
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await vi.runAllTimersAsync();
    });

    expect(calls.setInspectorTab).toContain('history');
    expect(calls.setInspectorTab).not.toContain('git');
  });

  test('initial restore writes terminal even when plan says browser', async () => {
    vi.useFakeTimers();
    layoutApi.preflight.mockResolvedValue(
      buildPlan({ workspaceView: 'browser', inspectorTab: 'files' }),
    );

    const { calls } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: null,
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await vi.runAllTimersAsync();
    });

    expect(layoutApi.preflight).toHaveBeenCalledTimes(1);
    expect(calls.setWorkspaceView).toContain('terminal');
    expect(calls.setWorkspaceView).not.toContain('browser');
  });

  test('initial restore with leftover browserTarget does not createPreview or switch view', async () => {
    vi.useFakeTimers();
    layoutApi.preflight.mockResolvedValue(
      buildPlan({
        workspaceView: 'browser',
        inspectorTab: 'files',
        actions: [
          { target: 'project', resourceId: 'p1', outcome: 'select' },
          { target: 'worktree', resourceId: 'w1', outcome: 'select' },
          { target: 'session', resourceId: 's1', outcome: 'select' },
          { target: 'workspaceView', resourceId: 'browser', outcome: 'select' },
          { target: 'inspectorTab', resourceId: 'files', outcome: 'select' },
          {
            target: 'browserTarget',
            resourceId: 'http://127.0.0.1:3000/',
            outcome: 'select',
          },
        ],
      }),
    );

    const { calls } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: 'w1',
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await vi.runAllTimersAsync();
    });

    expect(browserApi.createPreview).not.toHaveBeenCalled();
    expect(calls.setBrowserTargetUrl).toContain('http://127.0.0.1:3000/');
    expect(calls.setWorkspaceView).toContain('terminal');
    expect(calls.setWorkspaceView).not.toContain('browser');
  });

  test('initial restore only runs once across re-renders (restoreRanRef gate)', async () => {
    vi.useFakeTimers();
    layoutApi.preflight.mockResolvedValue(buildPlan({ workspaceView: 'files' }));

    const harness = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: null,
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await vi.runAllTimersAsync();
    });
    expect(layoutApi.preflight).toHaveBeenCalledTimes(1);

    // Re-render with same params; restoreRanRef should prevent second run.
    harness.rerender();
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    expect(layoutApi.preflight).toHaveBeenCalledTimes(1);
  });
});

describe('useWorkspaceSafeRestore — named snapshot apply preserves snapshot view', () => {
  beforeEach(() => {
    layoutApi.preflight.mockReset();
    layoutApi.apply.mockReset();
    layoutApi.save.mockReset();
    layoutApi.get.mockReset();
    layoutApi.listNamed.mockReset();
    layoutApi.deleteNamed.mockReset();
    layoutApi.apply.mockResolvedValue({
      restoreId: 'r1',
      status: 'complete',
      restoredCount: 0,
      skippedCount: 0,
      actions: [],
    });
    layoutApi.get.mockResolvedValue(null);
    layoutApi.save.mockImplementation(
      async (draft: { projectId: string; slotKey: string; kind: string; name?: string }) => ({
        schemaVersion: 1,
        id: 'autosave-1',
        revision: 1,
        slotKey: draft.slotKey,
        kind: draft.kind,
        name: draft.name ?? null,
        projectId: draft.projectId,
        activeWorktreeId: null,
        activeSessionId: null,
        workspaceView: 'terminal' as const,
        inspectorTab: 'files' as const,
        browserTargetUrl: null,
        createdAt: '2026-08-01T00:00:00.000Z',
        updatedAt: '2026-08-01T00:00:00.000Z',
      }),
    );
  });

  afterEach(() => {
    cleanup();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  test('applyNamedSnapshot writes snapshot workspaceView, not terminal', async () => {
    // Initial preflight returns terminal-ish plan.
    layoutApi.preflight.mockResolvedValueOnce(buildPlan({ workspaceView: 'terminal' }));
    // Named snapshot returns files view.
    layoutApi.preflight.mockResolvedValueOnce(buildPlan({ workspaceView: 'files' }));

    const { hook, calls } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: 'w1',
      activeSessionId: 's1',
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    // Wait for initial restore to settle (50ms suppress window inside runRestoreWithUi).
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 80));
    });
    expect(layoutApi.preflight).toHaveBeenCalledTimes(1);

    // Apply named snapshot whose plan.workspaceView = 'files'.
    await act(async () => {
      const promise = hook.current.applyNamedSnapshot('snap-1');
      // Advance the 50ms suppress window while applyNamedSnapshot is awaiting.
      await new Promise((resolve) => setTimeout(resolve, 80));
      await promise;
    });

    expect(layoutApi.preflight).toHaveBeenCalledTimes(2);
    // Named snapshot path must respect snapshot's workspaceView.
    expect(calls.setWorkspaceView).toContain('files');
    expect(calls.setWorkspaceView[calls.setWorkspaceView.length - 1]).toBe('files');
  });

  test('saveNamedSnapshot persists current selection as named layout', async () => {
    layoutApi.preflight.mockResolvedValueOnce(buildPlan({ workspaceView: 'terminal' }));
    layoutApi.save.mockResolvedValue({
      id: 'named-1',
      revision: 1,
      schemaVersion: 1,
      slotKey: 'named:test',
      kind: 'named',
      name: 'My Layout',
      projectId: 'p1',
      activeWorktreeId: 'w1',
      activeSessionId: 's1',
      workspaceView: 'files',
      inspectorTab: 'history',
      browserTargetUrl: null,
      createdAt: '2026-08-01T00:00:00.000Z',
      updatedAt: '2026-08-01T00:00:00.000Z',
    } as never);
    layoutApi.listNamed.mockResolvedValue([]);

    const { hook, calls } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: 'w1',
      activeSessionId: 's1',
      workspaceView: 'files',
      inspectorTab: 'history',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 80));
    });
    expect(layoutApi.preflight).toHaveBeenCalledTimes(1);

    await act(async () => {
      await hook.current.saveNamedSnapshot('My Layout');
      await new Promise((resolve) => setTimeout(resolve, 80));
    });

    expect(layoutApi.save).toHaveBeenCalled();
    const draft = layoutApi.save.mock.calls[0]?.[0] as {
      name: string;
      workspaceView: string;
      inspectorTab: string;
    };
    expect(draft.name).toBe('My Layout');
    // The hook autosaves the latest selection; the workspaceView passed in ('files')
    // is what the user currently sees.
    expect(draft.workspaceView).toBe('files');
    expect(draft.inspectorTab).toBe('history');
    // Initial restore forced terminal.
    expect(calls.setWorkspaceView).toContain('terminal');
  });
});

describe('useWorkspaceSafeRestore — apply failure rolls back previous view', () => {
  beforeEach(() => {
    layoutApi.preflight.mockReset();
    layoutApi.apply.mockReset();
    layoutApi.save.mockReset();
    layoutApi.get.mockReset();
    layoutApi.listNamed.mockReset();
    layoutApi.deleteNamed.mockReset();
    layoutApi.get.mockResolvedValue(null);
    layoutApi.save.mockImplementation(
      async (draft: { projectId: string; slotKey: string; kind: string; name?: string }) => ({
        schemaVersion: 1,
        id: 'autosave-1',
        revision: 1,
        slotKey: draft.slotKey,
        kind: draft.kind,
        name: draft.name ?? null,
        projectId: draft.projectId,
        activeWorktreeId: null,
        activeSessionId: null,
        workspaceView: 'terminal' as const,
        inspectorTab: 'files' as const,
        browserTargetUrl: null,
        createdAt: '2026-08-01T00:00:00.000Z',
        updatedAt: '2026-08-01T00:00:00.000Z',
      }),
    );
  });

  afterEach(() => {
    cleanup();
  });

  test('server apply failure rolls back to previous view, not forced terminal', async () => {
    // preflight returns files view but apply throws.
    layoutApi.preflight.mockResolvedValue(
      buildPlan({ workspaceView: 'files', inspectorTab: 'history' }),
    );
    layoutApi.apply.mockRejectedValue(new Error('apply boom'));

    const { calls, hook } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'old-p',
      activeWorktreeId: 'old-w',
      activeSessionId: null,
      workspaceView: 'browser', // previous view
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 80));
    });

    expect(layoutApi.apply).toHaveBeenCalledTimes(1);

    // On apply failure, the rollback path uses applySelectionSnapshot which
    // preserves the original previous.workspaceView = 'browser', NOT 'terminal'.
    expect(calls.setWorkspaceView).toContain('browser');
    expect(calls.setWorkspaceView).not.toContain('terminal');

    // restoreSummary should reflect partial status with applyFailed reason.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 80));
    });
    expect(hook.current.restoreSummary?.status).toBe('partial');
    expect(hook.current.restoreSummary?.reasons).toContain('applyFailed');
  });

  test('server apply revision conflict surfaces layoutRevisionChanged', async () => {
    layoutApi.preflight.mockResolvedValue(
      buildPlan({ workspaceView: 'files', inspectorTab: 'history' }),
    );
    layoutApi.apply.mockRejectedValue(
      Object.assign(new Error('workspace_layout_revision_changed'), {
        code: 'conflict',
      }),
    );

    const { hook } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'old-p',
      activeWorktreeId: 'old-w',
      activeSessionId: null,
      workspaceView: 'browser',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 80));
    });

    expect(hook.current.restoreSummary?.reasons).toEqual(['layoutRevisionChanged']);
  });

  test('autosave does not persist incomplete selection while restore preflight is in flight', async () => {
    vi.useFakeTimers();
    let resolvePreflight: ((plan: WorkspaceRestorePlan) => void) | undefined;
    layoutApi.preflight.mockImplementation(
      () =>
        new Promise<WorkspaceRestorePlan>((resolve) => {
          resolvePreflight = resolve;
        }),
    );

    makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: 'p1',
      activeWorktreeId: null,
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });
    expect(layoutApi.save).not.toHaveBeenCalled();

    await act(async () => {
      resolvePreflight?.(buildPlan({ workspaceView: 'files' }));
      await vi.runAllTimersAsync();
    });
    expect(layoutApi.apply).toHaveBeenCalledTimes(1);
  });

  test('url project skips slot restore when slot belongs to another project', async () => {
    vi.useFakeTimers();
    layoutApi.get.mockResolvedValue({
      schemaVersion: 1,
      id: 'slot-old',
      revision: 3,
      slotKey: 'desktop:auto:window:workbench-1',
      kind: 'auto',
      name: null,
      projectId: 'old-p',
      activeWorktreeId: 'w-old',
      activeSessionId: 's-old',
      workspaceView: 'files',
      inspectorTab: 'files',
      browserTargetUrl: null,
      createdAt: '2026-08-01T00:00:00.000Z',
      updatedAt: '2026-08-01T00:00:00.000Z',
    });

    const { calls } = makeHarness({
      projectsLoading: false,
      projectsLength: 1,
      activeProjectId: null,
      activeWorktreeId: null,
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      dirtyEditor: false,
      layoutSlotKey: 'desktop:auto:window:workbench-1',
      urlProjectId: 'url-p',
    });

    await act(async () => {
      await vi.runAllTimersAsync();
    });

    expect(layoutApi.preflight).not.toHaveBeenCalled();
    expect(layoutApi.apply).not.toHaveBeenCalled();
    expect(calls.selectProjectFromDeepLink).toEqual(['url-p']);
  });
});
