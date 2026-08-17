/**
 * @vitest-environment jsdom
 *
 * Business Logic（为什么需要）:
 *   运行时状态必须默认以一行摘要出现，WORKFLOW 向导入口不能藏进折叠区。
 *
 * Code Logic（做什么）:
 *   渲染带 running task 的 snapshot，断言向导按钮可见、详细列表默认不出现。
 */
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { OrchestratorRuntimeSnapshot } from '@/lib/types';
import { OrchestratorSnapshotBar } from './OrchestratorSnapshotBar';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, string | number>) => {
      if (!opts) return key;
      return `${key}:${Object.values(opts).join('|')}`;
    },
  }),
}));

afterEach(() => cleanup());

/**
 * Business Logic（为什么需要这个函数）:
 *   状态条测试需要一份含运行任务与事件的快照，才能区分摘要行和折叠详情。
 *
 * Code Logic（这个函数做什么）:
 *   返回最小合法 OrchestratorRuntimeSnapshot，并允许覆盖关键字段。
 */
function snapshot(
  overrides: Partial<OrchestratorRuntimeSnapshot> = {},
): OrchestratorRuntimeSnapshot {
  return {
    projectId: 'p1',
    projectKind: 'local',
    remoteStatus: 'local',
    generatedAt: '2026-08-17T04:00:00.000Z',
    latestTickAt: '2026-08-17T04:01:00.000Z',
    lastDispatchAt: '2026-08-17T04:01:00.000Z',
    lastDispatchedCount: 1,
    schedulerEnabled: true,
    workflowSource: 'WORKFLOW.md',
    workflowValid: true,
    workflowError: null,
    maxConcurrentTasks: 2,
    slotsUsed: 1,
    slotsAvailable: 1,
    latestError: null,
    runningTasks: [
      {
        taskId: 't1',
        title: 'Implement login',
        workflowState: 'inProgress',
        runState: 'running',
        attemptPhase: 'streaming',
        sessionId: null,
        worktreeId: null,
        lastRuntimeMessage: 'coding',
        lastActivityAt: null,
      },
    ],
    retryingTasks: [],
    recentEvents: [
      {
        id: 'e1',
        taskId: 't1',
        taskTitle: 'Implement login',
        kind: 'dispatch',
        message: 'started',
        createdAt: '2026-08-17T04:01:00.000Z',
      },
    ],
    ...overrides,
  };
}

describe('OrchestratorSnapshotBar', () => {
  test('keeps the workflow wizard on the compact strip and collapses run lists', () => {
    render(
      <OrchestratorSnapshotBar
        snapshot={snapshot()}
        remoteStatus={null}
        cachedAt={null}
        loading={false}
        errorMessage={null}
        showContent
        onRefresh={vi.fn()}
        onOpenSettings={vi.fn()}
        onOpenWorkflowWizard={vi.fn()}
      />,
    );

    expect(screen.getByTestId('open-workflow-wizard')).toBeTruthy();
    expect(screen.getByText('orchestrator:snapshot.schedulerEnabled')).toBeTruthy();
    expect(screen.getAllByText('orchestrator:snapshot.runningCount:1').length).toBeGreaterThan(0);
    const details = document.querySelector('details');
    expect(details).toBeTruthy();
    expect(details?.open).toBe(false);
  });

  test('keeps workflow and latest errors visible outside the disclosure', () => {
    render(
      <OrchestratorSnapshotBar
        snapshot={snapshot({
          workflowValid: false,
          workflowError: 'missing goal',
          latestError: 'tick failed',
          runningTasks: [],
          recentEvents: [],
        })}
        remoteStatus={null}
        cachedAt={null}
        loading={false}
        errorMessage={null}
        showContent
        onRefresh={vi.fn()}
        onOpenSettings={vi.fn()}
        onOpenWorkflowWizard={vi.fn()}
      />,
    );

    expect(screen.getByText('orchestrator:snapshot.workflowInvalid')).toBeTruthy();
    expect(screen.getByText('orchestrator:snapshot.workflowError:missing goal')).toBeTruthy();
    expect(screen.getByText('orchestrator:snapshot.latestError:tick failed')).toBeTruthy();
  });
});
