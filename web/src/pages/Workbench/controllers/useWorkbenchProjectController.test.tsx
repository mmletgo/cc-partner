// @vitest-environment jsdom
/**
 * useWorkbenchProjectController 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   在 controller 抽取后，项目级 "当前项目请求守卫" 和 "远端离线状态机" 必须独立可测；
 *   同时 launch summary 仅在「有项目未选中」时拉取，失败 section 隔离 / abort 清理必须锁定。
 *
 * Code Logic（这个测试做什么）:
 *   - 使用 @testing-library/react 的 renderHook 把 controller 挂在 React 树中；
 *   - 通过 rerender 修改 activeProject / projects 等输入，模拟项目切换；
 *   - mock workbenchApi.getLaunchSummary 断言 fetch gating、abort、error isolation。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

import type { WorkbenchProject } from '@/lib/types';
import type { WorkbenchLaunchSummaryWire } from '@/lib/types';

const getLaunchSummaryMock = vi.fn();

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    getLaunchSummary: (...args: unknown[]) => getLaunchSummaryMock(...args),
  },
}));

import { useWorkbenchProjectController } from './useWorkbenchProjectController';

const REMOTE_OFFLINE_ERROR = '远端设备不在线';

function buildLocalProject(overrides: Partial<WorkbenchProject> = {}): WorkbenchProject {
  return {
    id: 'local-1',
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

function buildRemoteProject(overrides: Partial<WorkbenchProject> = {}): WorkbenchProject {
  return {
    id: 'remote:device-a:abc',
    name: 'remote-app',
    kind: 'remote',
    deviceId: 'device-a',
    deviceName: 'Studio Mac',
    path: '/Users/hans/app',
    lastOpenedAt: '2026-07-01T00:00:00Z',
    createdAt: '2026-07-01T00:00:00Z',
    updatedAt: '2026-07-01T00:00:00Z',
    ...overrides,
  };
}

function buildLaunchWire(
  overrides: Partial<WorkbenchLaunchSummaryWire> = {},
): WorkbenchLaunchSummaryWire {
  return {
    projects: {
      kind: 'ready',
      value: [
        {
          id: 'local-1',
          name: 'local',
          kind: 'local',
          deviceId: 'self',
          deviceName: 'Mac',
          path: '/Users/hans/local',
          lastOpenedAt: '2026-07-01T00:00:00Z',
        },
      ],
    },
    sessions: { kind: 'ready', value: [] },
    tasks: { kind: 'ready', value: [] },
    transfers: { kind: 'error', message: 'transfer offline' },
    devices: { kind: 'ready', value: [] },
    generatedAt: '2026-07-14T12:00:00.000Z',
    ...overrides,
  };
}

interface ControllerProps {
  activeProject: WorkbenchProject | null;
  activeProjectId: string | null;
  projects: WorkbenchProject[];
  selectProject: (project: WorkbenchProject) => Promise<WorkbenchProject>;
}

function renderController(props: ControllerProps) {
  return renderHook(
    ({ activeProject, activeProjectId, projects, selectProject }) =>
      useWorkbenchProjectController({
        activeProject,
        activeProjectId,
        projects,
        selectProject,
      }),
    {
      initialProps: props,
    },
  );
}

/** 等待所有 pending microtask 落地（queueMicrotask / Promise.then）。 */
async function flushMicrotasks(rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i += 1) {
    await Promise.resolve();
  }
}

/** 等待 macrotask（visibility polling / setTimeout 0 / promise settle）。 */
async function flushMacrotasks(rounds = 4): Promise<void> {
  for (let i = 0; i < rounds; i += 1) {
    await act(async () => {
      await new Promise<void>((resolve) => {
        window.setTimeout(resolve, 0);
      });
    });
  }
}

beforeEach(() => {
  getLaunchSummaryMock.mockReset();
  getLaunchSummaryMock.mockResolvedValue(buildLaunchWire());
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => 'visible',
  });
});

afterEach(() => {
  cleanup();
});

describe('useWorkbenchProjectController', () => {
  test('current-project request success clears remote offline state', async () => {
    const remote = buildRemoteProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: remote,
      activeProjectId: remote.id,
      projects: [remote],
      selectProject,
    });

    // 先把远端项目置为离线。
    act(() => {
      result.current.markRequestFailure(remote.id, new Error(REMOTE_OFFLINE_ERROR));
    });
    expect(result.current.remoteProjectOffline).toBe(true);
    expect(result.current.remoteWriteDisabled).toBe(true);

    // 随后一次成功的远端读请求应清除离线提示。
    act(() => {
      result.current.markRequestSuccess(remote.id);
    });
    expect(result.current.remoteProjectOffline).toBe(false);
    expect(result.current.remoteWriteDisabled).toBe(false);
  });

  test('current-project request failure marks remote offline and disables writes', () => {
    const remote = buildRemoteProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: remote,
      activeProjectId: remote.id,
      projects: [remote],
      selectProject,
    });

    expect(result.current.remoteProjectOffline).toBe(false);

    act(() => {
      result.current.markRequestFailure(remote.id, new Error(REMOTE_OFFLINE_ERROR));
    });

    expect(result.current.remoteProjectOffline).toBe(true);
    expect(result.current.remoteWriteDisabled).toBe(true);
  });

  test('old-project request failure is ignored after switching projects', () => {
    const remoteA = buildRemoteProject({ id: 'remote:device-a:a' });
    const remoteB = buildRemoteProject({ id: 'remote:device-b:b' });
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result, rerender } = renderController({
      activeProject: remoteA,
      activeProjectId: remoteA.id,
      projects: [remoteA, remoteB],
      selectProject,
    });

    // 切到 B 之后，A 的 stale 错误响应到达。
    rerender({
      activeProject: remoteB,
      activeProjectId: remoteB.id,
      projects: [remoteA, remoteB],
      selectProject,
    });

    expect(result.current.isCurrentProject(remoteA.id)).toBe(false);
    expect(result.current.isCurrentProject(remoteB.id)).toBe(true);

    act(() => {
      // A 的 stale 错误不能把 B 标成离线，也不能让 B 进入只读。
      result.current.markRequestFailure(remoteA.id, new Error(REMOTE_OFFLINE_ERROR));
    });

    expect(result.current.remoteProjectOffline).toBe(false);
    expect(result.current.remoteWriteDisabled).toBe(false);
  });

  test('local project is never marked remote offline even on offline error', () => {
    const local = buildLocalProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: local,
      activeProjectId: local.id,
      projects: [local],
      selectProject,
    });

    act(() => {
      // 本机项目即便收到同样的离线错误文案也不应进入离线/只读状态。
      result.current.markRequestFailure(local.id, new Error(REMOTE_OFFLINE_ERROR));
    });

    expect(result.current.remoteProjectOffline).toBe(false);
    expect(result.current.remoteWriteDisabled).toBe(false);
  });

  test('unrelated error text does not mark remote project offline', () => {
    const remote = buildRemoteProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: remote,
      activeProjectId: remote.id,
      projects: [remote],
      selectProject,
    });

    act(() => {
      result.current.markRequestFailure(remote.id, new Error('读取终端失败'));
    });

    expect(result.current.remoteProjectOffline).toBe(false);
    expect(result.current.remoteWriteDisabled).toBe(false);
  });

  test('switching active project clears prior remote offline state', async () => {
    const remoteA = buildRemoteProject({ id: 'remote:device-a:a' });
    const remoteB = buildRemoteProject({ id: 'remote:device-b:b' });
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result, rerender } = renderController({
      activeProject: remoteA,
      activeProjectId: remoteA.id,
      projects: [remoteA, remoteB],
      selectProject,
    });

    act(() => {
      result.current.markRequestFailure(remoteA.id, new Error(REMOTE_OFFLINE_ERROR));
    });
    expect(result.current.remoteProjectOffline).toBe(true);

    // 切到 B：activeProjectId 变化触发 controller 内部 queueMicrotask 重置离线状态。
    rerender({
      activeProject: remoteB,
      activeProjectId: remoteB.id,
      projects: [remoteA, remoteB],
      selectProject,
    });
    await flushMicrotasks();

    expect(result.current.remoteProjectOffline).toBe(false);
    expect(result.current.remoteWriteDisabled).toBe(false);
  });

  test('markRequestSuccess for an unrelated project does not clear current offline state', () => {
    const remoteA = buildRemoteProject({ id: 'remote:device-a:a' });
    const remoteB = buildRemoteProject({ id: 'remote:device-b:b' });
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: remoteA,
      activeProjectId: remoteA.id,
      projects: [remoteA, remoteB],
      selectProject,
    });

    act(() => {
      result.current.markRequestFailure(remoteA.id, new Error(REMOTE_OFFLINE_ERROR));
    });
    expect(result.current.remoteProjectOffline).toBe(true);

    // 其他项目的成功响应不应清除当前项目的离线提示。
    act(() => {
      result.current.markRequestSuccess(remoteB.id);
    });
    expect(result.current.remoteProjectOffline).toBe(true);
  });

  test('selectProjectFromDeepLink resolves true and triggers selectProject when project exists', async () => {
    const local = buildLocalProject({ id: 'p1' });
    const remote = buildRemoteProject({ id: 'remote:device-a:r' });
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: local,
      activeProjectId: local.id,
      projects: [local, remote],
      selectProject,
    });

    let resolved: boolean | null = null;
    await act(async () => {
      resolved = await result.current.selectProjectFromDeepLink(remote.id);
    });

    expect(resolved).toBe(true);
    expect(selectProject).toHaveBeenCalledTimes(1);
    expect(selectProject.mock.calls[0]?.[0].id).toBe(remote.id);
  });

  test('selectProjectFromDeepLink resolves true without calling selectProject when already active', async () => {
    const local = buildLocalProject({ id: 'p1' });
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: local,
      activeProjectId: local.id,
      projects: [local],
      selectProject,
    });

    let resolved: boolean | null = null;
    await act(async () => {
      resolved = await result.current.selectProjectFromDeepLink(local.id);
    });

    expect(resolved).toBe(true);
    expect(selectProject).not.toHaveBeenCalled();
  });

  test('selectProjectFromDeepLink resolves false when project not found', async () => {
    const local = buildLocalProject({ id: 'p1' });
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: local,
      activeProjectId: local.id,
      projects: [local],
      selectProject,
    });

    let resolved: boolean | null = null;
    await act(async () => {
      resolved = await result.current.selectProjectFromDeepLink('does-not-exist');
    });

    expect(resolved).toBe(false);
    expect(selectProject).not.toHaveBeenCalled();
  });

  test('isCurrentProject tracks latest activeProjectId after rerender', () => {
    const local = buildLocalProject({ id: 'p1' });
    const remote = buildRemoteProject({ id: 'remote:device-a:r' });
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result, rerender } = renderController({
      activeProject: local,
      activeProjectId: local.id,
      projects: [local, remote],
      selectProject,
    });

    expect(result.current.isCurrentProject('p1')).toBe(true);
    expect(result.current.isCurrentProject('remote:device-a:r')).toBe(false);

    rerender({
      activeProject: remote,
      activeProjectId: remote.id,
      projects: [local, remote],
      selectProject,
    });

    expect(result.current.isCurrentProject('p1')).toBe(false);
    expect(result.current.isCurrentProject('remote:device-a:r')).toBe(true);
  });

  test('fetches launch summary only when projects exist and none selected', async () => {
    const local = buildLocalProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    const { result } = renderController({
      activeProject: null,
      activeProjectId: null,
      projects: [local],
      selectProject,
    });

    await flushMacrotasks();

    expect(getLaunchSummaryMock).toHaveBeenCalled();
    expect(result.current.launchSummary.projects.kind).toBe('ready');
    expect(result.current.launchSummary.transfers.kind).toBe('error');
    if (result.current.launchSummary.transfers.kind === 'error') {
      expect(result.current.launchSummary.transfers.message).toBe('transfer offline');
    }
  });

  test('does not fetch launch summary when zero projects', async () => {
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    renderController({
      activeProject: null,
      activeProjectId: null,
      projects: [],
      selectProject,
    });

    await flushMacrotasks();
    expect(getLaunchSummaryMock).not.toHaveBeenCalled();
  });

  test('does not fetch launch summary when active project selected', async () => {
    const local = buildLocalProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    renderController({
      activeProject: local,
      activeProjectId: local.id,
      projects: [local],
      selectProject,
    });

    await flushMacrotasks();
    expect(getLaunchSummaryMock).not.toHaveBeenCalled();
  });

  test('aborts in-flight launch summary when context becomes active project', async () => {
    const local = buildLocalProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    let resolveWire: ((value: WorkbenchLaunchSummaryWire) => void) | null = null;
    getLaunchSummaryMock.mockImplementation(
      () =>
        new Promise<WorkbenchLaunchSummaryWire>((resolve) => {
          resolveWire = resolve;
        }),
    );

    const { result, rerender } = renderController({
      activeProject: null,
      activeProjectId: null,
      projects: [local],
      selectProject,
    });

    await flushMicrotasks();
    expect(getLaunchSummaryMock).toHaveBeenCalledTimes(1);
    expect(result.current.launchSummary.projects.kind).toBe('loading');

    // 选中项目：应 abort，后续 resolve 不得写入。
    rerender({
      activeProject: local,
      activeProjectId: local.id,
      projects: [local],
      selectProject,
    });
    await flushMicrotasks();

    await act(async () => {
      resolveWire?.(buildLaunchWire());
      await flushMicrotasks();
    });

    // 离开 launch 模式后保持进入前/离开时的状态机结果；不得被 stale resolve 写成 ready。
    // 进入模式时已是 loading；abort 后仍 loading（未 mark ready）。
    expect(result.current.launchSummary.projects.kind).toBe('loading');
  });

  test('per-section error isolation in controller state', async () => {
    const local = buildLocalProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    getLaunchSummaryMock.mockResolvedValue(
      buildLaunchWire({
        projects: {
          kind: 'ready',
          value: [
            {
              id: local.id,
              name: local.name,
              kind: local.kind,
              deviceId: local.deviceId,
              deviceName: local.deviceName,
              path: local.path,
              lastOpenedAt: local.lastOpenedAt,
            },
          ],
        },
        sessions: { kind: 'error', message: 'sessions failed' },
        tasks: { kind: 'ready', value: [] },
        transfers: { kind: 'error', message: 'transfers failed' },
        devices: { kind: 'ready', value: [] },
      }),
    );

    const { result } = renderController({
      activeProject: null,
      activeProjectId: null,
      projects: [local],
      selectProject,
    });

    await flushMacrotasks();

    expect(result.current.launchSummary.projects.kind).toBe('ready');
    expect(result.current.launchSummary.sessions.kind).toBe('error');
    expect(result.current.launchSummary.tasks.kind).toBe('ready');
    expect(result.current.launchSummary.transfers.kind).toBe('error');
    expect(result.current.launchSummary.devices.kind).toBe('ready');
  });

  test('refresh failure marks ready sections stale and keeps values', async () => {
    const local = buildLocalProject();
    const selectProject = vi.fn(async (project: WorkbenchProject) => project);
    getLaunchSummaryMock.mockResolvedValueOnce(buildLaunchWire());

    const { result } = renderController({
      activeProject: null,
      activeProjectId: null,
      projects: [local],
      selectProject,
    });

    await flushMacrotasks();
    expect(result.current.launchSummary.projects.kind).toBe('ready');

    getLaunchSummaryMock.mockRejectedValueOnce(new Error('network offline'));
    await act(async () => {
      await result.current.refreshLaunchSummary();
    });

    expect(result.current.launchSummary.projects.kind).toBe('ready');
    if (result.current.launchSummary.projects.kind === 'ready') {
      expect(result.current.launchSummary.projects.stale).toBe(true);
      expect(result.current.launchSummary.projects.value[0]?.id).toBe(local.id);
    }
  });
});
