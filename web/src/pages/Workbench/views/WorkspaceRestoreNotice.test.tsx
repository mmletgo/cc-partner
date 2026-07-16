// @vitest-environment jsdom
/**
 * WorkspaceRestoreNotice 行为测试。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { WorkspaceRestoreNotice } from './WorkspaceRestoreNotice';
import type { WorkspaceRestoreSummary } from '../workspaceRestore';

afterEach(() => {
  cleanup();
});

function partialSummary(): WorkspaceRestoreSummary {
  return {
    restoreId: 'r1',
    status: 'partial',
    restoredCount: 3,
    skippedCount: 2,
    reasons: ['tmuxTargetMissing', 'sessionMissing'],
    silent: false,
    dirtyEditorPreserved: true,
  };
}

describe('WorkspaceRestoreNotice', () => {
  it('keeps a complete automatic restore silent and summarizes partial restore once', () => {
    const { container: complete } = render(
      <WorkspaceRestoreNotice
        summary={{
          restoreId: 'r',
          status: 'complete',
          restoredCount: 5,
          skippedCount: 0,
          reasons: [],
          silent: true,
          dirtyEditorPreserved: true,
        }}
        onDismiss={() => undefined}
      />,
    );
    expect(complete.querySelector('[role="status"]')).toBeNull();
    cleanup();

    const { container } = render(
      <WorkspaceRestoreNotice summary={partialSummary()} onDismiss={() => undefined} />,
    );
    expect(container.querySelectorAll('[role="status"]')).toHaveLength(1);
    expect(screen.getByText('已恢复 3 项，2 项已跳过')).toBeTruthy();
  });

  it('expands bounded reason codes and dismisses', () => {
    const onDismiss = vi.fn();
    const { container } = render(
      <WorkspaceRestoreNotice summary={partialSummary()} onDismiss={onDismiss} />,
    );
    const root = container.querySelector('[data-testid="workspace-restore-notice"]')!;
    fireEvent.click(within(root as HTMLElement).getByRole('button', { name: '查看原因' }));
    expect(within(root as HTMLElement).getByText('tmuxTargetMissing')).toBeTruthy();
    fireEvent.click(within(root as HTMLElement).getByRole('button', { name: '关闭' }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});
