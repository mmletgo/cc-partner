// @vitest-environment jsdom
/**
 * useWorkbenchAutomationController 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   在 controller 抽取后，自动化域的 staged deep link（project→worktree→session 三段式定位）、
 *   自动化控制台开/关、执行现场 takeover（OrchestratorPanel open-workbench 回跳）必须独立可测。
 *   这些行为在 Workbench.tsx 内曾由 deepLinkApplicationRef + 三个 staged effect +
 *   handleOpenAutomationTaskWorkbench 协作实现，本测试覆盖它们抽出后仍保持原有契约。
 *
 * Code Logic（这个测试做什么）:
 *   - 使用 @testing-library/react 的 renderHook 把 controller 挂在 React 树中；
 *   - 通过 rerender 修改 deepLink / activeProjectId / projects 等输入，模拟项目切换与 deep link 到达；
 *   - 调用 openAutomation / closeAutomation / applyAutomationDeepLink / openTaskWorkbench，
 *     断言 automationOpen、selectProjectFromDeepLink、setActiveWorktreeId、focusSession、navigate 调用日志。
 */
import { afterEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';

import { useWorkbenchAutomationController } from './useWorkbenchAutomationController';
import type { WorkbenchAutomationControllerParams } from './useWorkbenchAutomationController';
import type { WorkbenchDeepLink } from '../workbenchDeepLink';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import type { WorkbenchFileWorkspaceView } from '../workbenchFiles';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

function buildLocalProject(overrides: Partial<WorkbenchProject> = {}): WorkbenchProject {
  return {
    id: 'p1',
    name: 'local',
    kind: 'local',
    deviceId: 'self',
    deviceName: 'Mac',
    path: '/Users/hans/local',
    lastOpenedAt: '2026-07-01T00:00:00Z',
    createdAt: '2026-07-01T00:00:00Z',
    updatedAt: '2026-07-01T00:00:00Z',
    ...overrides,
  };
}

function buildWorktree(overrides: Partial<WorkbenchWorktree> = {}): WorkbenchWorktree {
  return {
    id: 'wt-main',
    projectId: 'p1',
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: '/Users/hans/local',
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
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-07-01T00:00:00Z',
    ...overrides,
  };
}

function buildSession(overrides: Partial<WorkbenchSession> = {}): WorkbenchSession {
  return {
    id: 's-target',
    projectId: 'p1',
    worktreeId: 'wt-main',
    name: 'target shell',
    command: 'bash',
    cwd: '/Users/hans/local',
    status: 'running',
    cols: 80,
    rows: 24,
    startedAt: '2026-07-01T00:00:00Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
    ...overrides,
  };
}

interface HarnessOverrides extends Partial<WorkbenchAutomationControllerParams> {
  deepLink?: WorkbenchDeepLink;
  locationSearch?: string;
  activeProjectId?: string | null;
  activeWorktreeId?: string | null;
  activeSessionId?: string | null;
  projects?: WorkbenchProject[];
  worktrees?: WorkbenchWorktree[];
  scopedSessions?: WorkbenchSession[];
  automationConsoleOpen?: boolean;
}

function buildHarness(overrides: HarnessOverrides = {}): WorkbenchAutomationControllerParams {
  const project = buildLocalProject();
  const worktree = buildWorktree();
  const session = buildSession();
  // 注意：activeProjectId/activeWorktreeId/activeSessionId 允许显式传 null，所以用
  // `overrides.X === undefined ? default : overrides.X` 形式而非 `??`，避免把 null 当 falsy 吃掉。
  return {
    deepLink: overrides.deepLink ?? { projectId: null, worktreeId: null, sessionId: null },
    locationSearch: overrides.locationSearch ?? '',
    activeProjectId:
      overrides.activeProjectId === undefined ? project.id : overrides.activeProjectId,
    activeWorktreeId:
      overrides.activeWorktreeId === undefined ? worktree.id : overrides.activeWorktreeId,
    activeSessionId:
      overrides.activeSessionId === undefined ? session.id : overrides.activeSessionId,
    projects: overrides.projects ?? [project],
    worktrees: overrides.worktrees ?? [worktree],
    scopedSessions: overrides.scopedSessions ?? [session],
    automationConsoleOpen: overrides.automationConsoleOpen ?? false,
    selectProjectFromDeepLink:
      overrides.selectProjectFromDeepLink ?? vi.fn(async () => true),
    setActiveWorktreeId:
      overrides.setActiveWorktreeId ??
      vi.fn((next: string | null | ((current: string | null) => string | null)) => {
        if (typeof next === 'function') next(null);
      }),
    focusSession: overrides.focusSession ?? vi.fn(async () => true),
    setAutomationConsoleOpen: overrides.setAutomationConsoleOpen ?? vi.fn(),
    requestWorkspaceView: overrides.requestWorkspaceView ?? vi.fn(),
    openFileByPath: overrides.openFileByPath ?? vi.fn(async () => true),
    navigate: overrides.navigate ?? vi.fn(),
  };
}

function renderController(params: WorkbenchAutomationControllerParams) {
  return renderHook((props: WorkbenchAutomationControllerParams) => useWorkbenchAutomationController(props), {
    initialProps: params,
  });
}

/** 等待所有 pending microtask 落地（queueMicrotask / Promise.then）。 */
async function flushMicrotasks(rounds = 8): Promise<void> {
  for (let i = 0; i < rounds; i += 1) {
    await Promise.resolve();
  }
}

afterEach(() => {
  cleanup();
});

describe('useWorkbenchAutomationController', () => {
  test('automationOpen reflects the injected automationConsoleOpen state', () => {
    const { result, rerender } = renderController(buildHarness({ automationConsoleOpen: false }));
    expect(result.current.automationOpen).toBe(false);

    rerender(buildHarness({ automationConsoleOpen: true }));
    expect(result.current.automationOpen).toBe(true);
  });

  test('openAutomation opens the console and requests the terminal workspace view', () => {
    const setAutomationConsoleOpen = vi.fn();
    const requestWorkspaceView = vi.fn();
    const { result } = renderController(
      buildHarness({ setAutomationConsoleOpen, requestWorkspaceView }),
    );

    act(() => {
      result.current.openAutomation();
    });

    expect(setAutomationConsoleOpen).toHaveBeenCalledWith(true);
    expect(requestWorkspaceView).toHaveBeenCalledWith('terminal' as WorkbenchFileWorkspaceView);
  });

  test('closeAutomation closes the console and requests the terminal workspace view', () => {
    const setAutomationConsoleOpen = vi.fn();
    const requestWorkspaceView = vi.fn();
    const { result } = renderController(
      buildHarness({ setAutomationConsoleOpen, requestWorkspaceView }),
    );

    act(() => {
      result.current.closeAutomation();
    });

    expect(setAutomationConsoleOpen).toHaveBeenCalledWith(false);
    expect(requestWorkspaceView).toHaveBeenCalledWith('terminal' as WorkbenchFileWorkspaceView);
  });

  test('openTaskWorkbench navigates, closes the console and returns to terminal', async () => {
    const navigate = vi.fn();
    const setAutomationConsoleOpen = vi.fn();
    const requestWorkspaceView = vi.fn();
    const { result } = renderController(
      buildHarness({ navigate, setAutomationConsoleOpen, requestWorkspaceView }),
    );

    await act(async () => {
      await result.current.openTaskWorkbench('/workbench?projectId=p1&worktreeId=wt-main&sessionId=s-target');
    });

    expect(navigate).toHaveBeenCalledWith('/workbench?projectId=p1&worktreeId=wt-main&sessionId=s-target');
    expect(setAutomationConsoleOpen).toHaveBeenCalledWith(false);
    expect(requestWorkspaceView).toHaveBeenCalledWith('terminal' as WorkbenchFileWorkspaceView);
  });

  test('project-only deep link selects the referenced project when it exists', async () => {
    const projectA = buildLocalProject({ id: 'p1' });
    const projectB = buildLocalProject({ id: 'p2', name: 'other' });
    const selectProjectFromDeepLink = vi.fn(async () => true);
    const { result } = renderController(
      buildHarness({
        deepLink: { projectId: 'p2', worktreeId: null, sessionId: null },
        locationSearch: '?projectId=p2',
        activeProjectId: 'p1',
        projects: [projectA, projectB],
        selectProjectFromDeepLink,
      }),
    );

    let resolved = false;
    await act(async () => {
      resolved = await result.current.applyAutomationDeepLink({ projectId: 'p2', worktreeId: null, sessionId: null });
    });

    expect(resolved).toBe(true);
    expect(selectProjectFromDeepLink).toHaveBeenCalledWith('p2');
  });

  test('applyAutomationDeepLink resolves true without selecting when project already active', async () => {
    const selectProjectFromDeepLink = vi.fn(async () => true);
    const { result } = renderController(
      buildHarness({
        activeProjectId: 'p1',
        projects: [buildLocalProject({ id: 'p1' })],
        selectProjectFromDeepLink,
      }),
    );

    let resolved = false;
    await act(async () => {
      resolved = await result.current.applyAutomationDeepLink({ projectId: 'p1', worktreeId: null, sessionId: null });
    });

    expect(resolved).toBe(true);
    expect(selectProjectFromDeepLink).not.toHaveBeenCalled();
  });

  test('applyAutomationDeepLink resolves false for a nonexistent project', async () => {
    const selectProjectFromDeepLink = vi.fn(async () => true);
    const { result } = renderController(
      buildHarness({
        activeProjectId: 'p1',
        projects: [buildLocalProject({ id: 'p1' })],
        selectProjectFromDeepLink,
      }),
    );

    let resolved = true;
    await act(async () => {
      resolved = await result.current.applyAutomationDeepLink({ projectId: 'missing', worktreeId: null, sessionId: null });
    });

    expect(resolved).toBe(false);
    expect(selectProjectFromDeepLink).not.toHaveBeenCalled();
  });

  test('applyAutomationDeepLink resolves true for an empty (no-op) target', async () => {
    const selectProjectFromDeepLink = vi.fn(async () => true);
    const { result } = renderController(buildHarness({ selectProjectFromDeepLink }));

    let resolved = false;
    await act(async () => {
      resolved = await result.current.applyAutomationDeepLink({ projectId: null, worktreeId: null, sessionId: null });
    });

    expect(resolved).toBe(true);
    expect(selectProjectFromDeepLink).not.toHaveBeenCalled();
  });

  test('staged deep link reactively focuses the referenced session after worktree selection', async () => {
    const project = buildLocalProject({ id: 'p1' });
    const mainWt = buildWorktree({ id: 'wt-main', projectId: 'p1' });
    const targetSession = buildSession({ id: 's-target', projectId: 'p1', worktreeId: 'wt-main' });
    const focusSession = vi.fn(async () => true);
    const setActiveWorktreeId = vi.fn((next: string | null | ((current: string | null) => string | null)) => { if (typeof next === "function") next(null); });

    const { rerender } = renderController(
      buildHarness({
        deepLink: { projectId: 'p1', worktreeId: 'wt-main', sessionId: 's-target' },
        locationSearch: '?projectId=p1&worktreeId=wt-main&sessionId=s-target',
        activeProjectId: 'p1',
        activeWorktreeId: null,
        activeSessionId: null,
        projects: [project],
        worktrees: [mainWt],
        scopedSessions: [],
        setActiveWorktreeId,
        focusSession,
      }),
    );

    // Stage 1→2: project 已对齐时 worktree 段应触发 setActiveWorktreeId。
    await waitFor(() => {
      expect(setActiveWorktreeId).toHaveBeenCalled();
    });

    // 模拟 worktree 已被选中、sessions 已加载后页面 rerender。
    rerender(
      buildHarness({
        deepLink: { projectId: 'p1', worktreeId: 'wt-main', sessionId: 's-target' },
        locationSearch: '?projectId=p1&worktreeId=wt-main&sessionId=s-target',
        activeProjectId: 'p1',
        activeWorktreeId: 'wt-main',
        activeSessionId: null,
        projects: [project],
        worktrees: [mainWt],
        scopedSessions: [targetSession],
        setActiveWorktreeId,
        focusSession,
      }),
    );
    await flushMicrotasks();

    // Stage 3: session 应被 focus。
    await waitFor(() => {
      expect(focusSession).toHaveBeenCalledWith('s-target');
    });
  });

  test('deep link with a nonexistent worktree does not call setActiveWorktreeId', async () => {
    const project = buildLocalProject({ id: 'p1' });
    const mainWt = buildWorktree({ id: 'wt-main', projectId: 'p1' });
    const setActiveWorktreeId = vi.fn((next: string | null | ((current: string | null) => string | null)) => { if (typeof next === "function") next(null); });

    renderController(
      buildHarness({
        deepLink: { projectId: 'p1', worktreeId: 'wt-missing', sessionId: null },
        locationSearch: '?projectId=p1&worktreeId=wt-missing',
        activeProjectId: 'p1',
        activeWorktreeId: null,
        projects: [project],
        worktrees: [mainWt],
        setActiveWorktreeId,
      }),
    );

    await flushMicrotasks();

    expect(setActiveWorktreeId).not.toHaveBeenCalled();
  });

  test('deep link with a nonexistent session does not call focusSession', async () => {
    const project = buildLocalProject({ id: 'p1' });
    const mainWt = buildWorktree({ id: 'wt-main', projectId: 'p1' });
    const otherSession = buildSession({ id: 's-other', projectId: 'p1', worktreeId: 'wt-main' });
    const focusSession = vi.fn(async () => true);

    renderController(
      buildHarness({
        deepLink: { projectId: 'p1', worktreeId: 'wt-main', sessionId: 's-missing' },
        locationSearch: '?projectId=p1&worktreeId=wt-main&sessionId=s-missing',
        activeProjectId: 'p1',
        activeWorktreeId: 'wt-main',
        activeSessionId: null,
        projects: [project],
        worktrees: [mainWt],
        scopedSessions: [otherSession],
        focusSession,
      }),
    );

    await flushMicrotasks();

    expect(focusSession).not.toHaveBeenCalled();
  });

  test('a newer deep link search cancels application of the older one', async () => {
    const projectA = buildLocalProject({ id: 'p1' });
    const projectB = buildLocalProject({ id: 'p2', name: 'other' });
    const targetWtB = buildWorktree({ id: 'wt-b', projectId: 'p2' });
    const focusSession = vi.fn(async () => true);
    const setActiveWorktreeId = vi.fn((next: string | null | ((current: string | null) => string | null)) => { if (typeof next === "function") next(null); });

    // 先注入 deep link A（指向 p1）。
    const { rerender } = renderController(
      buildHarness({
        deepLink: { projectId: 'p1', worktreeId: null, sessionId: null },
        locationSearch: '?projectId=p1',
        activeProjectId: 'p2',
        projects: [projectA, projectB],
        worktrees: [],
        scopedSessions: [],
        setActiveWorktreeId,
        focusSession,
      }),
    );

    // 还没等 A 完成就切到 deep link B（指向 p2 + worktree + session）。
    const targetSessionB = buildSession({ id: 's-b', projectId: 'p2', worktreeId: 'wt-b' });
    rerender(
      buildHarness({
        deepLink: { projectId: 'p2', worktreeId: 'wt-b', sessionId: 's-b' },
        locationSearch: '?projectId=p2&worktreeId=wt-b&sessionId=s-b',
        activeProjectId: 'p2',
        activeWorktreeId: 'wt-b',
        activeSessionId: null,
        projects: [projectA, projectB],
        worktrees: [targetWtB],
        scopedSessions: [targetSessionB],
        setActiveWorktreeId,
        focusSession,
      }),
    );

    await flushMicrotasks();

    // B 的 session 被 focus，A 的 staged 工作被新 search 取消。
    await waitFor(() => {
      expect(focusSession).toHaveBeenCalledWith('s-b');
    });
  });

  test('openAutomation does not touch terminal/session state', () => {
    const setActiveWorktreeId = vi.fn((next: string | null | ((current: string | null) => string | null)) => { if (typeof next === "function") next(null); });
    const focusSession = vi.fn(async () => true);
    const setAutomationConsoleOpen = vi.fn();
    const requestWorkspaceView = vi.fn();
    const { result } = renderController(
      buildHarness({ setActiveWorktreeId, focusSession, setAutomationConsoleOpen, requestWorkspaceView }),
    );

    act(() => {
      result.current.openAutomation();
    });

    expect(setActiveWorktreeId).not.toHaveBeenCalled();
    expect(focusSession).not.toHaveBeenCalled();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   WORKFLOW 向导 files deep link 必须在 project/worktree 就绪后调用 file controller bridge。
   *
   * Code Logic（这个测试做什么）:
   *   view=files&path=WORKFLOW.md 时关闭 automation、切 files 视图并 openFileByPath。
   */
  test('files deep link opens path via file controller after context is ready', async () => {
    const openFileByPath = vi.fn(async () => true);
    const setAutomationConsoleOpen = vi.fn();
    const requestWorkspaceView = vi.fn();
    const focusSession = vi.fn(async () => true);

    renderController(
      buildHarness({
        deepLink: {
          projectId: 'p1',
          worktreeId: null,
          sessionId: null,
          view: 'files',
          path: 'WORKFLOW.md',
        },
        locationSearch: '?projectId=p1&view=files&path=WORKFLOW.md',
        activeProjectId: 'p1',
        activeWorktreeId: 'wt-main',
        openFileByPath,
        setAutomationConsoleOpen,
        requestWorkspaceView,
        focusSession,
      }),
    );

    await waitFor(() => {
      expect(openFileByPath).toHaveBeenCalledWith('WORKFLOW.md');
    });
    expect(setAutomationConsoleOpen).toHaveBeenCalledWith(false);
    expect(requestWorkspaceView).toHaveBeenCalledWith('files');
    expect(focusSession).not.toHaveBeenCalled();
  });

  /**
   * Business Logic: Gate D Task6 禁止新增第八个 Workbench page controller。
   * Code Logic: 扫描 controllers 目录仍只有既有 7 个 useWorkbench*Controller。
   */
  test('does not introduce an eighth Workbench page controller', () => {
    const here = path.dirname(fileURLToPath(import.meta.url));
    const files = fs
      .readdirSync(here)
      .filter((name) => /^useWorkbench.*Controller\.ts$/.test(name));
    expect(files.sort()).toEqual(
      [
        'useWorkbenchAutomationController.ts',
        'useWorkbenchFileController.ts',
        'useWorkbenchProjectController.ts',
        'useWorkbenchPromptOptimizerController.ts',
        'useWorkbenchSessionSearchController.ts',
        'useWorkbenchTerminalController.ts',
        'useWorkbenchWorktreeGitController.ts',
      ].sort(),
    );
    expect(files).toHaveLength(7);
    expect(files.some((name) => /useWorkbenchController\.ts$/.test(name))).toBe(false);
  });
});
