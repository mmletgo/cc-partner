/**
 * 移动端自动化创建成功提示自动消失。
 *
 * Business Logic（为什么需要这个测试）:
 *   创建到 Backlog/Todo/Start 的确认只应短暂出现，不能一直钉在任务列表上方。
 *
 * Code Logic（这个测试做什么）:
 *   mock list/create transport；创建成功后立刻看到 status；推进 MOBILE_TRANSIENT_STATUS_MS 后清空。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { WorkbenchProject } from '@/lib/types';
import { MOBILE_TRANSIENT_STATUS_MS } from '../mobileTransientStatus';

const listViewsMock = vi.fn();
const createViewMock = vi.fn();
const listExperimentsMock = vi.fn();
const getRuntimeSnapshotMock = vi.fn();

vi.mock('@/api/workbenchHttp', () => ({
  createHttpOrchestratorClientRequestId: () => 'req-create-1',
  httpOrchestratorTransport: {
    tasks: {
      listViews: (...args: unknown[]) => listViewsMock(...args),
      createView: (...args: unknown[]) => createViewMock(...args),
      createBlock: vi.fn(),
      listEvidence: vi.fn(async () => []),
    },
    experiments: {
      list: (...args: unknown[]) => listExperimentsMock(...args),
    },
    getRuntimeSnapshot: (...args: unknown[]) => getRuntimeSnapshotMock(...args),
  },
}));

vi.mock('@/api/transferHttp', () => ({
  transferHttp: {
    listDevices: vi.fn(async () => []),
  },
}));

vi.mock('@/hooks/attentionInvalidation', () => ({
  requestAttentionInvalidation: vi.fn(),
}));

import { useMobileAutomationController } from './useMobileAutomationController';

const createdTask = {
  id: 't1',
  projectId: 'project-1',
  title: '标题',
  goal: '目标',
  acceptanceCriteria: '验收',
  status: 'draft',
  workflowState: 'backlog',
  runState: 'idle',
  attemptPhase: null,
  source: 'internal',
  externalId: null,
  externalIdentifier: null,
  externalUrl: null,
  externalState: null,
  externalLabels: null,
  runnerProvider: null,
  claudeSessionId: null,
  transcriptPath: null,
  runtimeStartedAt: null,
  lastActivityAt: null,
  lastRuntimeEvent: null,
  lastRuntimeMessage: null,
  priority: 0,
  branchName: null,
  worktreeId: null,
  sessionId: null,
  blockedReason: null,
  attempt: 0,
  createdAt: '2026-07-13T00:00:00.000Z',
  updatedAt: '2026-07-13T00:00:00.000Z',
  startedAt: null,
  finishedAt: null,
};

function wrapper({ children }: { children: ReactNode }) {
  return <I18nextProvider i18n={i18n}>{children}</I18nextProvider>;
}

function createProject(): WorkbenchProject {
  return {
    id: 'project-1',
    name: 'demo',
    kind: 'local',
    deviceId: 'd1',
    deviceName: 'Mac',
    path: '/tmp/demo',
    lastOpenedAt: '2026-07-14T00:00:00Z',
    createdAt: '2026-07-14T00:00:00Z',
    updatedAt: '2026-07-14T00:00:00Z',
  };
}

describe('useMobileAutomationController created status', () => {
  beforeAll(async () => {
    await i18n.changeLanguage('zh');
  });

  afterEach(() => {
    vi.useRealTimers();
    listViewsMock.mockReset();
    createViewMock.mockReset();
    listExperimentsMock.mockReset();
    getRuntimeSnapshotMock.mockReset();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   创建成功的确认必须自动消失，失败文案不能被同一 timer 清掉。
   *
   * Code Logic（这个测试做什么）:
   *   填三字段后 create backlog；status 在 delay 前可见，到期后为 null。
   */
  test('created task status auto-dismisses', async () => {
    listViewsMock.mockResolvedValue([]);
    listExperimentsMock.mockResolvedValue([]);
    getRuntimeSnapshotMock.mockRejectedValue(new Error('skip runtime'));
    createViewMock.mockResolvedValue({ origin: 'local', task: createdTask });

    const { result } = renderHook(
      () => useMobileAutomationController({ project: createProject() }),
      { wrapper },
    );
    await waitFor(() => expect(listViewsMock).toHaveBeenCalled());

    act(() => {
      result.current.createDialog.onTitleChange('标题');
      result.current.createDialog.onGoalChange('目标');
      result.current.createDialog.onAcceptanceCriteriaChange('验收');
    });
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    await act(async () => {
      result.current.createDialog.onCreateTask(
        'backlog',
        'workbench:mobile.automationPanel.createdBacklog',
      );
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(result.current.shell.status).toBe('任务已创建到 Backlog');

    act(() => {
      vi.advanceTimersByTime(MOBILE_TRANSIENT_STATUS_MS - 1);
    });
    expect(result.current.shell.status).toBe('任务已创建到 Backlog');
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current.shell.status).toBeNull();
  });
});
