// @vitest-environment jsdom
/**
 * OrchestratorTaskDrawer 合同测试（A0：无 Changes / review diff 产品面）
 */
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { OrchestratorTask } from '@/lib/types';
import { OrchestratorTaskDrawer } from './OrchestratorTaskDrawer';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string) => {
      const map: Record<string, string> = {
        'orchestrator:tabs.summary': 'Summary',
        'orchestrator:tabs.evidence': 'Evidence',
        'orchestrator:tabs.ariaLabel': 'Task tabs',
        'orchestrator:detail.drawerLabel': 'Task',
        'orchestrator:detail.close': 'Close',
        'orchestrator:detail.title': 'Detail',
        'orchestrator:detail.subtitle': 'Subtitle',
        'orchestrator:detail.goal': 'Goal',
        'orchestrator:detail.acceptanceCriteria': 'Acceptance',
        'orchestrator:detail.localTask': 'Local',
        'orchestrator:detail.unknown': 'Unknown',
        'orchestrator:detail.unassigned': 'Unassigned',
        'orchestrator:detail.workflowState': 'Workflow',
        'orchestrator:detail.legacyStatus': 'Status',
        'orchestrator:detail.runState': 'Run',
        'orchestrator:detail.attemptPhase': 'Phase',
        'orchestrator:detail.branch': 'Branch',
        'orchestrator:detail.attempt': 'Attempt',
        'orchestrator:detail.activeSession': 'Session',
        'orchestrator:evidence.title': 'Evidence',
        'orchestrator:evidence.subtitle': 'Evidence list',
        'orchestrator:evidence.loading': 'Loading evidence',
        'orchestrator:detail.deliver': 'Deliver',
        'orchestrator:detail.deliverDisabled': 'Deliver unavailable',
      };
      return map[key] ?? key;
    },
  }),
}));

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个夹具）:
 *   Drawer 测试需要最小合法 task。
 *
 * Code Logic（这个函数做什么）:
 *   返回 Human Review 态 OrchestratorTask 桩。
 */
function makeTask(): OrchestratorTask {
  return {
    id: 'task-1',
    projectId: 'project-1',
    title: 'Review task',
    goal: 'Goal text',
    acceptanceCriteria: 'Accept',
    priority: 0,
    status: 'waiting_human',
    workflowState: 'humanReview',
    runState: 'idle',
    attemptPhase: null,
    attempt: 1,
    worktreeId: null,
    sessionId: null,
    branchName: null,
    baseBranch: null,
    blockedReason: null,
    lastRuntimeMessage: null,
    claudeSessionId: null,
    transcriptPath: null,
    source: null,
    externalId: null,
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-01-01T00:00:00Z',
    startedAt: null,
    completedAt: null,
    stateVersion: 1,
  } as OrchestratorTask;
}

/**
 * Business Logic（为什么需要这个夹具）:
 *   为 Drawer 提供默认 props。
 *
 * Code Logic（这个函数做什么）:
 *   返回最小可渲染 props，可 override。
 */
function baseProps(overrides: Record<string, unknown> = {}) {
  return {
    selectedTask: makeTask(),
    selectedRenderableTask: {
      origin: 'local' as const,
      task: makeTask(),
      view: { origin: 'local' as const, task: makeTask() },
    },
    selectedTaskCanStart: false,
    selectedTaskCanComplete: false,
    selectedTaskCanRequestRework: true,
    selectedTaskShowDeliver: true,
    selectedTaskCanDeliver: true,
    selectedTaskCanCancel: false,
    selectedTaskCanControlBlocked: false,
    selectedTaskCanOpenWorkbench: false,
    selectedTaskProgressMessage: null,
    selectedTaskTerminalLabel: null,
    startingTaskId: null,
    completingTaskId: null,
    reworkingTaskId: null,
    deliveringTaskId: null,
    retryingTaskId: null,
    cancelingTaskId: null,
    evidenceItems: [],
    evidenceLoading: false,
    evidenceError: null,
    latestVerifierEvidence: null,
    latestRepairPromptEvidence: null,
    developmentAttemptEvidenceItems: [],
    detailTab: 'summary' as const,
    onDetailTabChange: vi.fn(),
    reworkDialogOpen: false,
    reworkError: null,
    onOpenReworkDialog: vi.fn(),
    onCloseReworkDialog: vi.fn(),
    onSubmitRework: vi.fn(),
    onClose: vi.fn(),
    onStart: vi.fn(),
    onCompleteAgentRun: vi.fn(),
    onOpenWorkbench: vi.fn(),
    onRetry: vi.fn(),
    onDeliver: vi.fn(),
    onCancel: vi.fn(),
    ...overrides,
  };
}

describe('OrchestratorTaskDrawer', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   A0 后抽屉只有 Summary/Evidence，不得再暴露 Changes tab。
   *
   * Code Logic（这个测试做什么）:
   *   渲染 summary，断言 Changes 文案不存在，Summary 存在。
   */
  it('renders summary without Changes tab', () => {
    render(<OrchestratorTaskDrawer {...(baseProps() as never)} />);
    expect(screen.getByRole('tab', { name: 'Summary' })).toBeTruthy();
    expect(screen.getByRole('tab', { name: 'Evidence' })).toBeTruthy();
    expect(screen.queryByRole('tab', { name: 'Changes' })).toBeNull();
    expect(screen.getAllByText('Review task').length).toBeGreaterThan(0);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Deliver 在可交付时必须可用，不依赖 digest。
   *
   * Code Logic（这个测试做什么）:
   *   断言 Deliver 按钮存在且未 disabled。
   */
  it('enables Deliver without review digest gate', () => {
    render(<OrchestratorTaskDrawer {...(baseProps() as never)} />);
    const deliver = screen.getByRole('button', { name: /Deliver|deliver|交付/i });
    expect(deliver).toBeTruthy();
    expect((deliver as HTMLButtonElement).disabled).toBe(false);
  });
});
