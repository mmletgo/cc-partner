/**
 * WorkbenchProjectRail 信息架构与可发现性契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   项目 rail 是旗舰 Workbench 入口：分区标题、空态说明、本机/局域网 CTA
 *   与可读状态文案必须稳定，且不引入新的项目 API。
 *
 * Code Logic（这个测试做什么）:
 *   用 MemoryRouter + I18nextProvider + 注入的 WorkbenchProjectsContext
 *   渲染 rail；断言标题、空态 CTA、键盘可读名称/状态；点击 CTA 复用
 *   chooseAndAddProject / 打开远端选择器回调。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { WorkbenchProject } from '@/lib/types';
import {
  WorkbenchProjectsContext,
  type WorkbenchProjectsContextValue,
} from '@/hooks/workbenchProjectsContext';
import { WorkbenchProjectRail } from './WorkbenchProjectRail';

const fleetMockState = {
  projectSummaries: {} as Record<
    string,
    {
      projectId: string;
      displayName: string;
      projectKind: string;
      agentCounts: {
        launching: number;
        working: number;
        needsInput: number;
        idle: number;
        completed: number;
        failed: number;
        disconnected: number;
      };
      attentionCount: number;
      terminalCount: number;
      gitState: 'clean' | 'dirty' | 'conflict' | 'unknown';
      browserState: 'active' | 'absent' | 'unknown';
      orchestratorRunning: number;
      orchestratorRetrying: number;
      lastActivityAt: string | null;
    }
  >,
  snapshot: null as null | {
    generatedAt: string;
    truncated: boolean;
    devices: Array<{
      deviceId: string;
      deviceName: string;
      reachability: 'live' | 'offline' | 'unsupported';
      freshness: 'live' | 'cached' | 'unknown';
      schedulerSlotsUsed: number | null;
      schedulerSlotsMax: number | null;
      projects: Array<{ projectId: string }>;
      errorCode: string | null;
      capturedAt: string | null;
    }>;
  },
};

vi.mock('@/hooks/useLanAgentFleet', () => ({
  useLanAgentFleet: () => ({
    snapshot: fleetMockState.snapshot,
    loading: false,
    error: null,
    refresh: async () => undefined,
    projectSummaries: fleetMockState.projectSummaries,
  }),
}));

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享最小项目 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 WorkbenchProject。
 */
function buildProject(overrides: Partial<WorkbenchProject> = {}): WorkbenchProject {
  return {
    id: 'proj-1',
    name: 'demo-app',
    path: '/Users/demo/demo-app',
    kind: 'local',
    deviceId: 'local',
    deviceName: '本机',
    lastOpenedAt: '2026-07-13T00:00:00.000Z',
    createdAt: '2026-07-13T00:00:00.000Z',
    updatedAt: '2026-07-13T00:00:00.000Z',
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需挂载路由、i18n 与项目上下文。
 *
 * Code Logic（这个函数做什么）:
 *   组装默认 mock context 并 render WorkbenchProjectRail。
 */
function renderRail(partial: Partial<WorkbenchProjectsContextValue> = {}) {
  const value: WorkbenchProjectsContextValue = {
    projects: [],
    activeProjectId: null,
    activeProject: null,
    projectsLoading: false,
    projectBusy: false,
    projectError: null,
    projectSessionStats: {},
    loadProjects: vi.fn(async () => undefined),
    refreshProjectSessionStats: vi.fn(async () => undefined),
    chooseAndAddProject: vi.fn(async () => null),
    openRemoteProject: vi.fn(async () => null),
    selectProject: vi.fn(async (project) => project),
    removeProject: vi.fn(async () => undefined),
    ...partial,
  };

  render(
    <I18nextProvider i18n={i18n}>
      <MemoryRouter>
        <WorkbenchProjectsContext.Provider value={value}>
          <WorkbenchProjectRail />
        </WorkbenchProjectsContext.Provider>
      </MemoryRouter>
    </I18nextProvider>,
  );

  return value;
}

describe('WorkbenchProjectRail discovery IA', () => {
  test('renders section heading and empty explanation with local/remote CTAs', () => {
    const ctx = renderRail({ projects: [] });

    expect(screen.getByRole('heading', { name: '工作台项目' })).toBeTruthy();
    expect(
      screen.getByText('添加本机或局域网项目后，可从侧栏随时进入工作台。'),
    ).toBeTruthy();

    const localCta = screen.getByRole('button', { name: '添加本机项目' });
    const remoteCta = screen.getByRole('button', { name: '选择局域网项目' });
    expect(localCta).toBeTruthy();
    expect(remoteCta).toBeTruthy();

    fireEvent.click(localCta);
    expect(ctx.chooseAndAddProject).toHaveBeenCalledTimes(1);

    fireEvent.click(remoteCta);
    expect(screen.getByRole('dialog', { name: '打开远端项目' })).toBeTruthy();
  });

  test('exposes project name and text status for keyboard users', () => {
    const active = buildProject({ id: 'active', name: 'active-repo' });
    const idle = buildProject({ id: 'idle', name: 'idle-repo', path: '/tmp/idle' });
    renderRail({
      projects: [active, idle],
      activeProjectId: active.id,
      activeProject: active,
    });

    expect(screen.getByRole('button', { name: /active-repo/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /idle-repo/ })).toBeTruthy();
    expect(screen.getByText('当前')).toBeTruthy();
    expect(screen.getByText('未选中')).toBeTruthy();
  });

  test('does not badge normal working agents but badges needs-input', () => {
    const project = buildProject({ id: 'p1', name: 'agent-repo' });
    fleetMockState.projectSummaries = {
      p1: {
        projectId: 'p1',
        displayName: 'agent-repo',
        projectKind: 'local',
        agentCounts: {
          launching: 0,
          working: 4,
          needsInput: 0,
          idle: 0,
          completed: 0,
          failed: 0,
          disconnected: 0,
        },
        attentionCount: 0,
        terminalCount: 1,
        gitState: 'clean',
        browserState: 'absent',
        orchestratorRunning: 0,
        orchestratorRetrying: 0,
        lastActivityAt: null,
      },
    };
    const { rerender } = render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <WorkbenchProjectsContext.Provider
            value={{
              projects: [project],
              activeProjectId: project.id,
              activeProject: project,
              projectsLoading: false,
              projectBusy: false,
              projectError: null,
              projectSessionStats: {},
              loadProjects: vi.fn(async () => undefined),
              chooseAndAddProject: vi.fn(async () => null),
              openRemoteProject: vi.fn(async () => null),
              selectProject: vi.fn(async () => undefined),
              removeProject: vi.fn(async () => undefined),
              setActiveProjectId: vi.fn(),
              addProjectFromPath: vi.fn(async () => null),
              refreshProjectSessionStats: vi.fn(async () => undefined),
            } as unknown as WorkbenchProjectsContextValue}
          >
            <WorkbenchProjectRail />
          </WorkbenchProjectsContext.Provider>
        </MemoryRouter>
      </I18nextProvider>,
    );

    expect(screen.queryByLabelText(/需要处理/)).toBeNull();
    expect(screen.getByRole('link', { name: /Fleet|局域网 Agent Fleet/ })).toBeTruthy();

    fleetMockState.projectSummaries = {
      p1: {
        ...fleetMockState.projectSummaries.p1!,
        agentCounts: {
          launching: 0,
          working: 4,
          needsInput: 1,
          idle: 0,
          completed: 0,
          failed: 0,
          disconnected: 0,
        },
      },
    };
    rerender(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <WorkbenchProjectsContext.Provider
            value={{
              projects: [project],
              activeProjectId: project.id,
              activeProject: project,
              projectsLoading: false,
              projectBusy: false,
              projectError: null,
              projectSessionStats: {},
              loadProjects: vi.fn(async () => undefined),
              chooseAndAddProject: vi.fn(async () => null),
              openRemoteProject: vi.fn(async () => null),
              selectProject: vi.fn(async () => undefined),
              removeProject: vi.fn(async () => undefined),
              setActiveProjectId: vi.fn(),
              addProjectFromPath: vi.fn(async () => null),
              refreshProjectSessionStats: vi.fn(async () => undefined),
            } as unknown as WorkbenchProjectsContextValue}
          >
            <WorkbenchProjectRail />
          </WorkbenchProjectsContext.Provider>
        </MemoryRouter>
      </I18nextProvider>,
    );
    expect(screen.getByLabelText('1 个 Agent 需要处理')).toBeTruthy();
  });
});
