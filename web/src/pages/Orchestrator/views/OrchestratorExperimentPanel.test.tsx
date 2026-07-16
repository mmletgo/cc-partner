/**
 * @vitest-environment jsdom
 */
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { OrchestratorExperiment } from '@/lib/types/orchestrator';
import { OrchestratorExperimentPanel } from './OrchestratorExperimentPanel';

/**
 * Business Logic（为什么需要这个函数）:
 *   单测需要稳定的 NeedsDecision 实验样本。
 *
 * Code Logic（做什么）:
 *   返回含推荐 winner 与两个 ready candidate 的 DTO。
 */
function awaitingApprovalExperiment(): OrchestratorExperiment {
  return {
    id: 'exp-1',
    projectId: 'p1',
    title: '登录态恢复',
    goal: '实现登录态恢复',
    acceptance: '通过测试',
    status: 'needsDecision',
    selectionPolicy: 'comparative',
    maxParallel: 2,
    winnerTaskId: 'task-1',
    selectionReason: '推荐最小改动方案',
    confidence: 'medium',
    version: 1,
    createdAt: 't',
    updatedAt: 't',
    candidates: [
      {
        experimentId: 'exp-1',
        taskId: 'task-1',
        ordinal: 1,
        providerId: 'claudeCodeVisible',
        strategyLabel: 'minimal',
        outcome: 'candidateReady',
        createdAt: 't',
        updatedAt: 't',
      },
      {
        experimentId: 'exp-1',
        taskId: 'task-2',
        ordinal: 2,
        providerId: 'codexVisible',
        strategyLabel: 'refactor',
        outcome: 'candidateReady',
        createdAt: 't',
        updatedAt: 't',
      },
    ],
  };
}

describe('OrchestratorExperimentPanel', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   NeedsDecision 只提供一个推荐动作，禁止 Diff 审查控件。
   */
  it('shows one recommended action and no diff review controls', () => {
    render(
      <OrchestratorExperimentPanel
        experiment={awaitingApprovalExperiment()}
        onApproveRecommended={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getAllByRole('button', { name: '采用推荐' })).toHaveLength(1);
    expect(screen.queryByText(/Changes|Diff|批注/)).toBeNull();
  });
});
