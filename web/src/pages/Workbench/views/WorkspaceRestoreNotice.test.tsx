// @vitest-environment jsdom
/**
 * WorkspaceRestoreNotice 行为测试。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import i18n from 'i18next';
import { I18nextProvider, initReactI18next } from 'react-i18next';
import type { ReactElement } from 'react';
import { WorkspaceRestoreNotice } from './WorkspaceRestoreNotice';
import {
  TRANSIENT_RESTORE_NOTICE_MS,
  type WorkspaceRestoreSummary,
} from '../workspaceRestore';

// Business Logic: 组件按钮文案走 workbench:workspaceRestore.* i18n；测试需提供该 namespace，
// 否则 t() 回落为原始 key，getByRole({ name }) 无法命中。
const resources = {
  zh: {
    workbench: {
      workspaceRestore: {
        close: '关闭',
        viewReasons: '查看原因',
        collapseReasons: '收起原因',
      },
    },
  },
};

void i18n.use(initReactI18next).init({
  lng: 'zh',
  resources,
  interpolation: { escapeValue: false },
});

function wrap(ui: ReactElement): ReactElement {
  return <I18nextProvider i18n={i18n}>{ui}</I18nextProvider>;
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
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
      wrap(
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
      ),
    );
    expect(complete.querySelector('[role="status"]')).toBeNull();
    cleanup();

    const { container } = render(
      wrap(<WorkspaceRestoreNotice summary={partialSummary()} onDismiss={() => undefined} />),
    );
    expect(container.querySelectorAll('[role="status"]')).toHaveLength(1);
    expect(screen.getByText('已恢复 3 项，2 项已跳过')).toBeTruthy();
  });

  it('auto-dismisses only not-requested skips after a short notice', () => {
    vi.useFakeTimers();
    const onDismiss = vi.fn();
    render(
      wrap(
        <WorkspaceRestoreNotice
          summary={{
            restoreId: 'r-benign',
            status: 'partial',
            restoredCount: 5,
            skippedCount: 1,
            reasons: ['browserSkippedForNonBrowserView'],
            silent: false,
            dirtyEditorPreserved: true,
          }}
          onDismiss={onDismiss}
        />,
      ),
    );
    expect(screen.getByTestId('workspace-restore-notice')).toBeTruthy();
    expect(onDismiss).not.toHaveBeenCalled();
    vi.advanceTimersByTime(TRANSIENT_RESTORE_NOTICE_MS - 1);
    expect(onDismiss).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(onDismiss).toHaveBeenCalledTimes(1);
    cleanup();

    onDismiss.mockClear();
    render(wrap(<WorkspaceRestoreNotice summary={partialSummary()} onDismiss={onDismiss} />));
    vi.advanceTimersByTime(TRANSIENT_RESTORE_NOTICE_MS * 2);
    expect(onDismiss).not.toHaveBeenCalled();
  });

  it('expands bounded reason codes and dismisses', () => {
    const onDismiss = vi.fn();
    const { container } = render(
      wrap(<WorkspaceRestoreNotice summary={partialSummary()} onDismiss={onDismiss} />),
    );
    const root = container.querySelector('[data-testid="workspace-restore-notice"]')!;
    fireEvent.click(within(root as HTMLElement).getByRole('button', { name: '查看原因' }));
    expect(within(root as HTMLElement).getByText('tmuxTargetMissing')).toBeTruthy();
    fireEvent.click(within(root as HTMLElement).getByRole('button', { name: '关闭' }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});
