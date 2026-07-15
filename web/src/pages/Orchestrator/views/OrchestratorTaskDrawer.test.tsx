// @vitest-environment jsdom
/**
 * OrchestratorTaskDrawer review Changes / Deliver 合同测试
 *
 * Business Logic（为什么需要这个测试）:
 *   Human Review 抽屉在 diff error 时必须保留 Evidence 与 Rework，Deliver 可见但禁用；
 *   防止错误态吞掉审阅动作。
 *
 * Code Logic（这个测试做什么）:
 *   jsdom 渲染纯 props 抽屉；断言 error/alert、Deliver disabled、Rework enabled、Evidence tab。
 */
import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { OrchestratorTask } from '@/lib/types';
import {
  OrchestratorTaskDrawer,
  type OrchestratorTaskDrawerProps,
} from './OrchestratorTaskDrawer';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) => {
      const map: Record<string, string> = {
        'orchestrator:detail.deliver': '交付',
        'orchestrator:detail.requestRework': '要求返工',
        'orchestrator:tabs.evidence': 'Evidence',
        'orchestrator:tabs.summary': 'Summary',
        'orchestrator:tabs.changes': 'Changes',
        'orchestrator:tabs.ariaLabel': 'Task detail sections',
        'orchestrator:evidence.title': 'Evidence',
        'orchestrator:review.retry': '重试加载',
        'orchestrator:detail.drawerLabel': '当前任务',
        'orchestrator:detail.close': '关闭任务详情',
        'orchestrator:detail.title': '任务详情',
        'orchestrator:detail.subtitle': 'subtitle',
        'orchestrator:detail.goal': '目标',
        'orchestrator:detail.acceptanceCriteria': '验收标准',
        'orchestrator:detail.localTask': '本机任务',
        'orchestrator:workflow.humanReview': '人工复核',
        'orchestrator:run.idle': '空闲',
        'orchestrator:status.done': '已完成',
        'orchestrator:detail.workflowState': '工作流状态',
        'orchestrator:detail.legacyStatus': '兼容状态',
        'orchestrator:detail.runState': '运行状态',
        'orchestrator:detail.attemptPhase': '尝试阶段',
        'orchestrator:detail.unknown': '未知',
        'orchestrator:detail.branch': '分支',
        'orchestrator:detail.attempt': '尝试次数',
        'orchestrator:detail.noAttempt': '尚未开始',
        'orchestrator:detail.activeSession': '执行现场',
        'orchestrator:detail.unassigned': '未绑定',
        'orchestrator:detail.runnerProvider': 'Runner',
        'orchestrator:detail.claudeSession': 'Claude 会话',
        'orchestrator:detail.transcript': '转录文件',
        'orchestrator:detail.createdAt': '创建时间',
        'orchestrator:detail.updatedAt': '更新时间',
        'orchestrator:detail.lastActivity': '最后活动',
        'orchestrator:detail.lastEvent': '最后事件',
        'orchestrator:detail.lastMessage': '最后消息',
        'orchestrator:evidence.subtitle': 'subtitle',
        'orchestrator:evidence.loading': 'loading',
        'orchestrator:evidence.emptyTitle': 'empty',
        'orchestrator:evidence.emptyBody': 'empty body',
      };
      if (key === 'orchestrator:review.fileSummary' && opts) {
        return `+${opts.additions} / -${opts.deletions}`;
      }
      return map[key] ?? key;
    },
    i18n: { language: 'zh' },
  }),
}));

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   抽屉测试需要稳定的 Human Review 任务 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回 done + humanReview + idle 的 OrchestratorTask。
 */
function makeHumanReviewTask(): OrchestratorTask {
  return {
    id: 'task-1',
    projectId: 'project-1',
    title: 'Review me',
    goal: 'Ship feature',
    acceptanceCriteria: 'Tests pass',
    status: 'done',
    workflowState: 'humanReview',
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
    branchName: 'agent/task-1',
    worktreeId: 'wt-1',
    sessionId: 'sess-1',
    blockedReason: null,
    attempt: 1,
    createdAt: '2026-07-14T00:00:00.000Z',
    updatedAt: '2026-07-14T00:00:00.000Z',
    startedAt: null,
    finishedAt: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   纯视图测试只需覆盖目标 props，其余用稳定默认值。
 *
 * Code Logic（这个函数做什么）:
 *   合并 partial override 到完整 OrchestratorTaskDrawerProps。
 */
function makeTaskDrawerProps(
  overrides: Partial<OrchestratorTaskDrawerProps> = {},
): OrchestratorTaskDrawerProps {
  const task = overrides.selectedTask ?? makeHumanReviewTask();
  return {
    selectedTask: task,
    selectedRenderableTask: {
      origin: 'local',
      task,
      view: { origin: 'local', task },
      deviceId: null,
      deviceName: null,
    },
    selectedTaskCanStart: false,
    selectedTaskCanComplete: false,
    selectedTaskCanRequestRework: true,
    selectedTaskShowDeliver: true,
    selectedTaskCanDeliver: false,
    selectedTaskCanCancel: false,
    selectedTaskCanControlBlocked: false,
    selectedTaskCanOpenWorkbench: true,
    selectedTaskProgressMessage: null,
    selectedTaskTerminalLabel: 'sess-1',
    startingTaskId: null,
    completingTaskId: null,
    reworkingTaskId: null,
    deliveringTaskId: null,
    retryingTaskId: null,
    cancelingTaskId: null,
    evidenceItems: [
      {
        id: 'e1',
        taskId: task.id,
        kind: 'verificationReview',
        title: 'Verifier',
        summary: 'passed',
        content: 'ok',
        createdAt: '2026-07-14T00:00:00.000Z',
      },
    ],
    evidenceLoading: false,
    evidenceError: null,
    latestVerifierEvidence: null,
    latestRepairPromptEvidence: null,
    developmentAttemptEvidenceItems: [],
    detailTab: 'changes',
    onDetailTabChange: vi.fn(),
    reviewDiffState: 'error',
    reviewDiff: null,
    reviewDiffError: 'unavailable',
    selectedReviewFilePath: null,
    onSelectReviewFilePath: vi.fn(),
    onRetryReviewDiff: vi.fn(),
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

describe('OrchestratorTaskDrawer review actions', () => {
  test('diff error leaves evidence and review actions available', async () => {
    render(
      <OrchestratorTaskDrawer
        {...makeTaskDrawerProps({
          selectedTask: makeHumanReviewTask(),
          reviewDiffState: 'error',
          reviewDiffError: 'unavailable',
          detailTab: 'changes',
          selectedTaskShowDeliver: true,
          selectedTaskCanDeliver: false,
          selectedTaskCanRequestRework: true,
        })}
      />,
    );

    expect(await screen.findByText('unavailable')).toBeTruthy();
    const deliver = screen.getByRole('button', { name: '交付' }) as HTMLButtonElement;
    expect(deliver.disabled).toBe(true);
    const rework = screen.getByRole('button', { name: '要求返工' }) as HTMLButtonElement;
    expect(rework.disabled).toBe(false);
    expect(screen.getByRole('tab', { name: 'Evidence' })).toBeTruthy();
  });

  test('ready digest enables deliver and mounts only selected file patch', () => {
    const onSelect = vi.fn();
    render(
      <OrchestratorTaskDrawer
        {...makeTaskDrawerProps({
          reviewDiffState: 'ready',
          reviewDiffError: null,
          selectedTaskCanDeliver: true,
          selectedReviewFilePath: 'src/a.ts',
          onSelectReviewFilePath: onSelect,
          reviewDiff: {
            taskId: 'task-1',
            baseRef: 'main',
            headRef: 'worktree',
            totalFiles: 2,
            truncated: false,
            reviewDigest: 'digest-a',
            files: [
              {
                path: 'src/a.ts',
                status: 'modified',
                additions: 1,
                deletions: 0,
                patch: 'PATCH_A_ONLY',
                binary: false,
                truncated: false,
              },
              {
                path: 'src/b.ts',
                status: 'modified',
                additions: 2,
                deletions: 1,
                patch: 'PATCH_B_MUST_NOT_MOUNT',
                binary: false,
                truncated: false,
              },
            ],
          },
        })}
      />,
    );

    const deliver = screen.getByRole('button', { name: '交付' }) as HTMLButtonElement;
    expect(deliver.disabled).toBe(false);
    expect(screen.getByText('PATCH_A_ONLY')).toBeTruthy();
    expect(screen.queryByText('PATCH_B_MUST_NOT_MOUNT')).toBeNull();
    fireEvent.click(screen.getByText('src/b.ts', { exact: false }));
    expect(onSelect).toHaveBeenCalled();
  });
});
